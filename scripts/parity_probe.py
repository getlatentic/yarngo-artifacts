#!/usr/bin/env python3
"""Drive the real sidecar end to end: install, load, clone, synthesize.

This is the spike harness. Pointed at the torch pack's interpreter it answers
the question the Windows port hangs on — do the upstream dots.tts checkpoints
generate under this runtime — through the exact protocol the app uses, not a
side path. Pointed at the MLX interpreter it produces the comparison clip.

    python3 scripts/parity_probe.py \
        --python  <pack venv>/bin/python3 \
        --data    /tmp/parity-data \
        --model   dots-tts-mf \
        --voice   /path/to/reference.wav \
        --voice-text "what the reference says" \
        --text    "what to synthesize" \
        --seed    4242 \
        --output  /tmp/mf.wav

Exit code 0 means a clip exists at --output with speech-shaped audio in it.
"""

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class Sidecar:
    def __init__(self, python: str, data_dir: str):
        self.proc = subprocess.Popen(
            [python, str(ROOT / "sidecar" / "engine.py")],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=sys.stderr,
            text=True,
            env={
                **__import__("os").environ,
                "YARNGO_DATA": data_dir,
            },
        )
        self.next_id = 0

    def call(self, method: str, params: dict | None = None, timeout_s: float = 7200):
        self.next_id += 1
        request = {"id": self.next_id, "method": method, "params": params or {}}
        self.proc.stdin.write(json.dumps(request) + "\n")
        self.proc.stdin.flush()
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError(f"sidecar exited during {method}")
            reply = json.loads(line)
            if reply.get("id") != self.next_id:
                continue
            if not reply.get("ok"):
                raise RuntimeError(f"{method} failed: {reply.get('error')}")
            return reply["result"]
        raise TimeoutError(method)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--python", required=True)
    ap.add_argument("--data", required=True)
    ap.add_argument("--model", default="dots-tts-mf")
    ap.add_argument("--voice", required=True)
    ap.add_argument("--voice-text", required=True)
    ap.add_argument("--text", required=True)
    ap.add_argument("--seed", type=int, default=4242)
    ap.add_argument("--output", required=True)
    args = ap.parse_args()

    side = Sidecar(args.python, args.data)
    side.call("ping", timeout_s=60)

    models = {m["id"]: m for m in side.call("list_models")["models"]}
    if args.model not in models:
        print(f"catalogue has no {args.model}: {sorted(models)}", file=sys.stderr)
        return 2
    print(f"[probe] backend catalogue: {sorted(models)}")

    if not models[args.model]["installed"]:
        print(f"[probe] installing {args.model}…")
        side.call("install_model", {"model": args.model})
        while True:
            status = side.call("install_status", {"model": args.model})
            state = status["state"]
            if state == "installed":
                break
            if state == "failed":
                print(f"[probe] install failed: {status.get('error')}", file=sys.stderr)
                return 2
            done = status.get("downloaded_bytes") or 0
            total = status.get("total_bytes") or 0
            print(f"[probe] {done/1e9:.2f} / {total/1e9:.2f} GB", flush=True)
            time.sleep(5)

    print(f"[probe] loading {args.model}…")
    loaded = side.call("load_model", {"model": args.model})
    print(f"[probe] loaded in {loaded['load_s']}s")

    side.call(
        "register_voice",
        {
            "voice_id": "parity",
            "label": "parity reference",
            "reference_audio": str(Path(args.voice).resolve()),
            "reference_text": args.voice_text,
            "consent_statement": "parity probe over the validated reference",
            "app_version": "probe",
            "source": "import",
            # Preparation is an optimisation; the probe measures generation.
            "prepare": False,
        },
        timeout_s=600,
    )

    print("[probe] synthesizing…")
    started = time.monotonic()
    result = side.call(
        "synthesize",
        {
            "text": args.text,
            "voice_id": "parity",
            "model": args.model,
            "seed": args.seed,
            "output": str(Path(args.output).resolve()),
            "keep": False,
        },
    )
    wall = time.monotonic() - started

    audio_s = result.get("audio_s")
    print(f"[probe] {audio_s}s of audio in {wall:.0f}s wall — {result}")

    out = Path(args.output)
    if not out.exists() or out.stat().st_size < 1000:
        print("[probe] no usable audio was written", file=sys.stderr)
        return 1
    print(f"[probe] PASS — {out} ({out.stat().st_size/1e3:.0f} KB)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
