"""Install the official portable Ollama CPU runtime, without GPU libraries."""
import argparse
import hashlib
import json
import shutil
import urllib.request
import zipfile
from pathlib import Path

VERSION = "0.33.3"
ASSET = "ollama-windows-amd64.zip"


def cpu_file(name):
    parts = name.lower().replace("\\", "/").split("/")
    gpu_prefixes = ("cuda", "vulkan", "rocm", "cublas", "cudart", "nvrtc", "nvjit", "hipblas", "rocblas", "ggml-cuda", "ggml-vulkan", "ggml-hip")
    return not any(part.startswith(gpu_prefixes) for part in parts)


def install(home):
    dest = home / "ollama"
    marker = dest / "callhelper-runtime.json"
    if (dest / "ollama.exe").is_file() and marker.is_file():
        if json.loads(marker.read_text())["version"] == VERSION:
            print("Portable Ollama CPU runtime already installed.", flush=True)
            return
    api = f"https://api.github.com/repos/ollama/ollama/releases/tags/v{VERSION}"
    request = urllib.request.Request(api, headers={"User-Agent": "CallHelper-free-voice-setup"})
    with urllib.request.urlopen(request, timeout=60) as response:
        release = json.load(response)
    asset = next(a for a in release["assets"] if a["name"] == ASSET)
    expected_url = f"https://github.com/ollama/ollama/releases/download/v{VERSION}/{ASSET}"
    if asset["browser_download_url"] != expected_url:
        raise RuntimeError("Unexpected Ollama download URL.")
    archive = home / f"ollama-{VERSION}.zip"
    if not archive.exists() or archive.stat().st_size != asset["size"]:
        if shutil.disk_usage(home).free < asset["size"] + 512 * 1024 * 1024:
            raise RuntimeError("Not enough disk space for the Ollama runtime download.")
        print("Downloading official Ollama portable runtime (about 1.5 GB)...", flush=True)
        with urllib.request.urlopen(expected_url, timeout=60) as response, archive.open("wb") as output:
            shutil.copyfileobj(response, output, 1024 * 1024)
    digest = hashlib.file_digest(archive.open("rb"), "sha256").hexdigest()
    expected_digest = asset.get("digest")
    if expected_digest and expected_digest != "sha256:" + digest:
        archive.unlink()
        raise RuntimeError("Ollama download checksum mismatch. Rerun setup.")
    dest.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive) as package:
        selected = [info for info in package.infolist() if not info.is_dir() and cpu_file(info.filename)]
        if shutil.disk_usage(home).free < sum(info.file_size for info in selected) + 256 * 1024 * 1024:
            raise RuntimeError("Not enough space to extract the CPU runtime.")
        for info in selected:
            output = (dest / info.filename).resolve()
            if not output.is_relative_to(dest.resolve()):
                raise RuntimeError("Unsafe archive path.")
            output.parent.mkdir(parents=True, exist_ok=True)
            with package.open(info) as source, output.open("wb") as target:
                shutil.copyfileobj(source, target)
    if not (dest / "ollama.exe").is_file():
        raise RuntimeError("The official archive did not contain ollama.exe.")
    marker.write_text(json.dumps({"version": VERSION, "sha256": digest, "source": expected_url, "cpuOnly": True}), encoding="utf-8")
    archive.unlink()
    print("Portable Ollama CPU runtime installed; archive removed.", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--home", required=True, type=Path)
    args = parser.parse_args()
    install(args.home.resolve())
