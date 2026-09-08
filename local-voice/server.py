"""English streaming speech for Call Helper. Binds loopback; no cloud STT."""
import argparse
import array
import json
import os
import sys
import threading
from http import HTTPStatus
from pathlib import Path

PROTOCOL = 1
SAMPLE_RATE = 16000
MAX_SAMPLES = SAMPLE_RATE * 121


class TranscriptState:
    """Replace hypotheses by line id; never append duplicate interim text."""
    def __init__(self):
        self.lines = {}

    def update(self, line_id, text):
        self.lines[line_id] = text.strip()
        return self.text

    @property
    def text(self):
        return " ".join(text for text in self.lines.values() if text)


def pcm_floats(data):
    if not isinstance(data, bytes) or len(data) % 2:
        raise ValueError("Expected little-endian PCM16 audio.")
    pcm = array.array("h", data)
    if sys.byteorder != "little":
        pcm.byteswap()
    return [sample / 32768.0 for sample in pcm]


def download(home):
    # Download only during explicit setup. Serving never calls a downloader.
    os.environ["MOONSHINE_VOICE_CACHE"] = str(home / "models" / "moonshine")
    from moonshine_voice import get_model_for_language, ModelArch
    model_path, model_arch = get_model_for_language("en", ModelArch.TINY_STREAMING)
    manifest = {"modelPath": str(Path(model_path).resolve()), "modelArch": int(model_arch)}
    (home / "speech-model.json").write_text(json.dumps(manifest), encoding="utf-8")
    print("English speech model installed.", flush=True)


def serve(home):
    from moonshine_voice import Transcriber, TranscriptEventListener, ModelArch
    from websockets.sync.server import serve as ws_serve
    from websockets.exceptions import ConnectionClosed

    manifest = json.loads((home / "speech-model.json").read_text(encoding="utf-8-sig"))
    transcriber = Transcriber(
        model_path=manifest["modelPath"], model_arch=ModelArch(manifest["modelArch"])
    )
    active = threading.Lock()

    def process_request(connection, request):
        if request.headers.get("Origin") is not None:
            return connection.respond(HTTPStatus.FORBIDDEN, "Native local clients only.\n")
        if request.path == "/health":
            response = connection.respond(HTTPStatus.OK, json.dumps({
                "service": "callhelper-local-speech", "protocol": PROTOCOL, "ready": True
            }))
            del response.headers["Content-Type"]
            response.headers["Content-Type"] = "application/json"
            return response
        if request.path != "/transcribe":
            return connection.respond(HTTPStatus.NOT_FOUND, "Not found.\n")
        return None

    def handle(connection):
        if not active.acquire(blocking=False):
            connection.send(json.dumps({"type": "error", "message": "Speech is busy."}))
            return
        stream = None
        try:
            state = TranscriptState()

            class Listener(TranscriptEventListener):
                error = None
                connected = True

                def update(self, event, final=False):
                    text = state.update(event.line.line_id, event.line.text)
                    if self.connected:
                        try:
                            connection.send(json.dumps({"type": "transcript", "text": text, "final": final}))
                        except ConnectionClosed:
                            self.connected = False

                def on_line_started(self, event):
                    self.update(event)

                def on_line_text_changed(self, event):
                    self.update(event)

                def on_line_completed(self, event):
                    self.update(event, True)

                def on_error(self, event):
                    self.error = event.error

            listener = Listener()
            stream = transcriber.create_stream(update_interval=0.5)
            stream.add_listener(listener)
            stream.start()
            connection.send(json.dumps({"type": "ready", "protocol": PROTOCOL}))
            samples = 0
            while True:
                frame = connection.recv(timeout=130)
                if isinstance(frame, bytes):
                    audio = pcm_floats(frame)
                    samples += len(audio)
                    if samples > MAX_SAMPLES:
                        raise ValueError("Recording exceeds the two-minute limit.")
                    stream.add_audio(audio, SAMPLE_RATE)
                    if listener.error is not None:
                        raise RuntimeError("Speech inference failed.") from listener.error
                elif json.loads(frame) == {"type": "finish"}:
                    stream.stop()
                    if listener.error is not None:
                        raise RuntimeError("Final speech inference failed.") from listener.error
                    connection.send(json.dumps({"type": "done", "text": state.text}))
                    break
                else:
                    raise ValueError("Unknown speech command.")
        except ConnectionClosed:
            pass
        except Exception as error:
            # No audio or transcript is written to logs.
            print(f"Speech session failed: {type(error).__name__}", file=sys.stderr, flush=True)
            try:
                connection.send(json.dumps({"type": "error", "message": "Local transcription failed."}))
            except ConnectionClosed:
                pass
        finally:
            try:
                if stream is not None:
                    stream.remove_all_listeners()
                    stream.close()
            finally:
                active.release()

    print("Call Helper local speech ready on 127.0.0.1:8765", flush=True)
    try:
        with ws_serve(handle, "127.0.0.1", 8765, process_request=process_request,
                      origins=[None], max_size=64000, max_queue=8,
                      ping_interval=None, close_timeout=2) as server:
            server.serve_forever()
    finally:
        transcriber.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--home", required=True, type=Path)
    parser.add_argument("--download-only", action="store_true")
    args = parser.parse_args()
    home = args.home.resolve()
    home.mkdir(parents=True, exist_ok=True)
    if args.download_only:
        download(home)
    else:
        serve(home)


if __name__ == "__main__":
    main()
