"""Is a Python control thread schedulable while the speech backend works?

A protocol reader thread can only answer `ping` or a cancellation during a long
operation if the interpreter actually schedules it. This measures that directly:
a heartbeat thread records its own scheduling lag while each blocking path runs.

It proves schedulability, not thread safety, and not that the real reader and
writer path keeps up — those need a round-trip test across the process boundary.

MLX evaluates lazily, so a timing harness can measure graph construction and
attribute it to inference. Every path here materialises its result and reports
how long that took separately, so the split is visible rather than assumed.

    python tools/benchmark_sidecar_responsiveness.py > docs/results/<backend>.json
"""
import json, os, platform, statistics, subprocess, sys, threading, time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "sidecar"))
os.environ.setdefault("YARNGO_DATA", "/Users/dev/Library/Application Support/Yarngo Studio")
import engine  # noqa: E402

TICK = 0.05
lags: list[float] = []
stop = threading.Event()


def heartbeat() -> None:
    while not stop.is_set():
        t0 = time.monotonic()
        time.sleep(TICK)
        lags.append(time.monotonic() - t0 - TICK)


def _arrays(value, depth: int = 0, seen: set | None = None) -> list:
    """Every MLX array reachable from `value`, however it is wrapped.

    Walking rather than evaluating the return value directly: the paths worth
    measuring hand back a wrapper, a float, or nothing at all, and evaluating
    those measures the harness rather than the backend.
    """
    import mlx.core as mx
    seen = seen if seen is not None else set()
    if depth > 4 or id(value) in seen:
        return []
    seen.add(id(value))
    if isinstance(value, mx.array):
        return [value]
    if isinstance(value, (list, tuple, set)):
        return [a for v in value for a in _arrays(v, depth + 1, seen)]
    if isinstance(value, dict):
        return [a for v in value.values() for a in _arrays(v, depth + 1, seen)]
    if hasattr(value, "__dict__"):
        return [a for v in vars(value).values() for a in _arrays(v, depth + 1, seen)]
    return []


def materialise(value) -> tuple[float, int]:
    """Force every reachable lazy array, reporting the cost and how many.

    The count matters: a near-zero cost means "already evaluated" only when
    something was actually found to evaluate. It is also how the walk reports
    its own blind spots — a whole model yielding one array means the parameters
    were not reached, and that path's timing says nothing about readiness.
    """
    import mlx.core as mx
    found = _arrays(value)
    t0 = time.perf_counter()
    if found:
        mx.eval(found)
    return time.perf_counter() - t0, len(found)


def measure(name: str, fn, reps: int = 1, retained=None) -> dict:
    """`retained` names what the engine kept, for paths that return nothing
    useful — model weights, or the conditioning cache — so materialisation is
    measured against the objects that actually hold the work."""
    runs = []
    for _ in range(reps):
        lags.clear()
        t0 = time.monotonic()
        out = fn()
        call_s = time.monotonic() - t0
        eval_s, arrays = materialise(retained() if retained else out)
        runs.append({
            "call_s": round(call_s, 3),
            "materialise_s": round(eval_s, 6),
            "arrays_evaluated": arrays,
            "ticks_seen": len(lags),
            "ticks_expected": round(call_s / TICK),
            "lag_ms": {
                "p50": round(statistics.median(lags) * 1000, 1) if lags else None,
                "p95": round(sorted(lags)[int(len(lags) * 0.95)] * 1000, 1) if len(lags) > 20 else None,
                "max": round(max(lags) * 1000, 1) if lags else None,
            },
        })
    return {"operation": name, "runs": runs}


def main() -> int:
    threading.Thread(target=heartbeat, daemon=True).start()
    time.sleep(0.5)
    idle_max = round(max(lags) * 1000, 1)

    engine._load_voices_from_disk()
    model_id = engine._default_model()
    spec = engine.MODELS[model_id]
    holder: dict = {}
    results = [measure("model_load", lambda: holder.setdefault("m", engine._load(model_id)),
                       retained=lambda: holder["m"])]
    model = holder["m"]

    def conditioning_cache():
        gen = getattr(model, "_generator", model)
        return getattr(gen, "_prompt_cache", None)
    kwargs = dict(spec.get("gen") or {})

    voice_id = next(iter(engine._voices), None)
    if voice_id:
        results.append(measure("voice_conditioning_first_use",
                               lambda: engine._prepare_voice(voice_id, model_id),
                               retained=conditioning_cache))

    text = "The quick brown fox jumps over the lazy dog."
    results.append(measure("synthesis_default_voice",
                           lambda: model.generate(text, **kwargs), reps=3))
    if voice_id:
        v = engine._voices[voice_id]
        results.append(measure("synthesis_cloned_voice", lambda: model.generate(
            text, reference_audio=v["reference_audio"],
            reference_text=v["reference_text"], **kwargs), reps=3))
    stop.set()

    def version(pkg: str) -> str | None:
        # `mlx.__version__` does not exist; the distribution metadata does.
        try:
            from importlib.metadata import version as dist_version
            return dist_version(pkg)
        except Exception:
            return None

    print(json.dumps({
        "what": "python control-thread scheduling lag during blocking backend calls",
        "proves": "schedulability of a protocol reader thread on this configuration only",
        "does_not_prove": [
            "MLX thread safety",
            "that the real stdin reader and stdout writer keep up",
            "end-to-end cancellation acknowledgement",
            "any other backend, model, machine or MLX version",
        ],
        "host": {
            "machine": subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"],
                                      capture_output=True, text=True).stdout.strip() or platform.machine(),
            "memory_gb": round(int(subprocess.run(["sysctl", "-n", "hw.memsize"],
                                                  capture_output=True, text=True).stdout or 0) / 1e9, 1),
            "platform": f"{platform.system().lower()}-{platform.machine()}",
            "python": platform.python_version(),
            "mlx": version("mlx"),
        },
        "model": {"id": model_id, "repo": spec.get("repo"), "revision": spec.get("revision")},
        "input_text_chars": len(text),
        "tick_interval_s": TICK,
        "idle_lag_ms_max": idle_max,
        "results": results,
    }, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
