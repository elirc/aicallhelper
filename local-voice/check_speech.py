"""Opt-in real Moonshine test using a 16 kHz PCM16 practice WAV."""
import argparse
import json
import time
import wave
from websockets.sync.client import connect

parser = argparse.ArgumentParser()
parser.add_argument("wav")
args = parser.parse_args()
with wave.open(args.wav, "rb") as source:
    assert (source.getframerate(), source.getnchannels(), source.getsampwidth()) == (16000, 1, 2)
    chunks = []
    while chunk := source.readframes(2048):
        chunks.append(chunk)

partials = []
with connect("ws://127.0.0.1:8765/transcribe", open_timeout=10, ping_interval=None) as socket:
    assert json.loads(socket.recv(timeout=10)) == {"type": "ready", "protocol": 1}
    started = time.perf_counter()
    for chunk in chunks:
        socket.send(chunk)
        time.sleep(len(chunk) / 32000)
        while True:
            try:
                frame = json.loads(socket.recv(timeout=0))
            except TimeoutError:
                break
            assert frame["type"] == "transcript", frame
            if frame["text"]:
                partials.append({"atMs": round((time.perf_counter() - started) * 1000), "text": frame["text"]})
    stopped = time.perf_counter()
    socket.send(json.dumps({"type": "finish"}))
    while True:
        frame = json.loads(socket.recv(timeout=10))
        if frame["type"] == "done":
            text = frame["text"]
            break
        assert frame["type"] == "transcript", frame
    finalize_ms = round((time.perf_counter() - stopped) * 1000)

assert partials, "No live transcript arrived during recording."
assert "customer" in text.lower(), text
assert finalize_ms < 5000, f"Speech finalization exceeded the app's limit: {finalize_ms}ms"
print(json.dumps({"transcript": text, "livePartials": partials, "sttFinalizeMs": finalize_ms}, indent=2))
