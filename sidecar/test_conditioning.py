"""What voice deletion must do to the conditioning held in memory.

Deleting a voice is meant to take the derived conditioning with it. The first
implementation looked for the cache on the model object; this backend keeps it
on the generator inside an adapter, so the lookup found nothing, cleared
nothing, and reported success. Nothing failed — which is why it survived.

The checks that would have caught it are the ones that distinguish outcomes:

    cleared              something was found and removed
    already_empty        nothing to remove, which is normal and idempotent
    UnsupportedCacheLayout   a model is loaded and its cache cannot be found

Only the third is a failure, and it is the one that must never be silent: it
means conditioning may still be reachable and this process cannot prove it is
not. The caller's answer to that is to end the process.

Run against either pack's environment:

    <pack>/.venv/bin/python3 sidecar/test_conditioning.py

The backend itself is only exercised when asked, because loading a model and
conditioning a voice costs a minute:

    YARNGO_TEST_BACKEND=1 <pack>/.venv/bin/python3 sidecar/test_conditioning.py
"""

import os
import sys
import threading
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import engine  # noqa: E402


class RecordingLock:
    """A lock that remembers whether it was held when the cache was touched."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self.held = False
        self.taken = 0

    def __enter__(self):
        self._lock.acquire()
        self.held = True
        self.taken += 1
        return self

    def __exit__(self, *_exc):
        self.held = False
        self._lock.release()
        return False


class WatchedCache(dict):
    def __init__(self, lock: RecordingLock, entries: dict) -> None:
        super().__init__(entries)
        self._lock = lock
        self.cleared_while_locked: bool | None = None

    def clear(self) -> None:
        self.cleared_while_locked = self._lock.held
        super().clear()


class Generator:
    def __init__(self, cache) -> None:
        self._prompt_cache = cache
        self._prompt_cache_lock = cache._lock if isinstance(cache, WatchedCache) else None


class Adapter:
    """Shaped like the real one: the cache sits a level in, not on the model."""

    def __init__(self, cache) -> None:
        self._generator = Generator(cache)


class Bare:
    """A backend with no cache anywhere the engine knows to look."""


def check(name: str, condition: bool, failures: list) -> None:
    if not condition:
        failures.append(name)


def main() -> int:
    failures: list[str] = []
    original = engine._models

    # 1-3. A conditioned cache is found where this backend actually keeps it,
    #      and comes back empty.
    lock = RecordingLock()
    cache = WatchedCache(lock, {"waveform-a": object(), "waveform-b": object()})
    engine._models = {"m": Adapter(cache)}
    result = engine._forget_conditioning()
    check("finds the cache behind the adapter", len(cache) == 0, failures)
    check("reports how many entries went", result.entries_removed == 2, failures)
    check("reports cleared", result.status == "cleared", failures)
    check("says the scope was every voice", result.scope_applied == "all", failures)

    # 6. And touches it under the lock the backend guards it with.
    check("clears under the cache's own lock", cache.cleared_while_locked is True, failures)
    check("took that lock exactly once", lock.taken == 1, failures)

    # 4. Doing it again is a normal, successful no-op, not an error.
    again = engine._forget_conditioning()
    check("an empty cache is already_empty", again.status == "already_empty", failures)
    check("an empty cache removes nothing", again.entries_removed == 0, failures)

    # A model that was never loaded has nothing to search, which is also normal.
    engine._models = {}
    none_loaded = engine._forget_conditioning()
    check("no model loaded is already_empty", none_loaded.status == "already_empty", failures)

    # 5, 7. A backend whose cache cannot be located is the dangerous case, and
    #       must raise rather than report success or be swallowed.
    engine._models = {"m": Bare()}
    try:
        engine._forget_conditioning()
        failures.append("an unlocatable cache reported success")
    except engine.UnsupportedCacheLayout:
        pass
    except Exception as exc:  # noqa: BLE001
        failures.append(f"an unlocatable cache raised the wrong error: {exc!r}")

    # The other supported shapes still resolve.
    for label, model in (("cache directly on the model", Generator(WatchedCache(RecordingLock(), {"w": 1}))),):
        engine._models = {"m": model}
        check(f"finds a {label}", engine._forget_conditioning().entries_removed == 1, failures)

    engine._models = original

    checks = 12
    if os.environ.get("YARNGO_TEST_BACKEND"):
        checks += 2
        engine._load_voices_from_disk()
        voice_id = next(iter(engine._voices), None)
        if voice_id is None:
            failures.append("no voice enrolled to condition")
        else:
            model_id = engine._default_model()
            model = engine._load(model_id)
            engine._prepare_voice(voice_id, model_id)
            real, _ = engine._conditioning_cache(model)
            check("the real backend caches conditioning", len(real) > 0, failures)
            engine._forget_conditioning()
            check("the real cache comes back empty", len(real) == 0, failures)

    for failure in failures:
        print(f"FAIL: {failure}", file=sys.stderr)
    print(f"{checks - len(failures)}/{checks} conditioning checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
