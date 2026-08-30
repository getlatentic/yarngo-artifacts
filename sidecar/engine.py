"""MLX speech sidecar: line-delimited JSON over stdin/stdout.

The Rust app owns the UI and the process; this owns the models. One request per
line in, one response per line out, so the protocol stays debuggable by hand:

    {"id":1,"method":"synthesize","params":{...}}
    {"id":1,"ok":true,"result":{...}}

Models stay resident between requests — loading is the expensive part, and a
desktop app should pay it once. Voice profiles are cached for the same reason:
the first generation for a speaker costs roughly 43s against 15s thereafter,
and that difference is entirely reference processing.
"""

from __future__ import annotations

import contextlib
import json
import os
import shutil
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import platform
import random
import difflib
import math
import re
import threading

import numpy as np
import soundfile as sf

import protocol

# Voices live on disk so they survive a restart. Preparing a voice costs about
# 40 seconds, and asking the user to repeat that every launch is not an option.
# The app passes YARNGO_DATA when it spawns this process, so both sides agree
# on one directory rather than each deriving its own. `VOICESTUDIO_DATA` is the
# older name, still read so an existing setup keeps working.
VOICE_DIR = Path(
    os.environ.get(
        "YARNGO_DATA",
        os.environ.get(
            "VOICESTUDIO_DATA",
            Path.home() / "Library" / "Application Support" / "Yarngo Studio",
        ),
    )
) / "voices"

def _log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


# The catalogue is data, in `catalog.json`, so a model can be added or a
# revision corrected without shipping a new application. Order of preference:
# a fetched copy in the data directory, then the one bundled beside this file.
# The bundled copy is the floor — a fetched catalogue that fails any check
# below is ignored, loudly, and the app carries on with what it shipped with.
#
# `gen` values in that file were validated across 8 Nigerian speakers: 24/24
# identity separation, median 0.0% WER. Do not change them without re-running
# the sanity harness in voice-clone-bench.
CATALOG_API = 1

# Fields without which an entry cannot be used, so a truncated or hand-edited
# catalogue fails here rather than at the first generation.
REQUIRED_FIELDS = ("label", "name", "repo", "revision", "licence")


def _catalog_paths() -> list[Path]:
    here = Path(__file__).resolve().parent
    named = os.environ.get("YARNGO_CATALOG")
    return [
        # Where the application says it is. A runtime running its own engine
        # from its own directory cannot find this by looking around itself.
        *([Path(named)] if named else []),
        VOICE_DIR.parent / "catalog.json",       # fetched, if one has arrived
        here / "catalog.json",                   # bundled beside the sidecar
        here.parent / "catalog.json",            # bundled in Resources/
        here.parent / "packaging" / "catalog.json",  # a development checkout
    ]


def _read_catalog(path: Path, backend: str) -> dict | None:
    """Validate a catalogue file, or explain why it was refused."""
    try:
        payload = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        _log(f"catalogue {path} unreadable: {exc}")
        return None
    if payload.get("schema") != 1:
        _log(f"catalogue {path} has schema {payload.get('schema')!r}, expected 1")
        return None
    # A catalogue written for a newer sidecar may describe entries this one
    # cannot honour, so it is refused rather than half-understood.
    if int(payload.get("sidecar_api", 0)) > CATALOG_API:
        _log(f"catalogue {path} needs sidecar api {payload.get('sidecar_api')}, have {CATALOG_API}")
        return None
    models = (payload.get("backends") or {}).get(backend)
    if not isinstance(models, dict) or not models:
        _log(f"catalogue {path} has nothing for the {backend} backend")
        return None
    for model_id, entry in models.items():
        missing = [f for f in REQUIRED_FIELDS if not entry.get(f)]
        if missing:
            _log(f"catalogue {path}: {model_id} is missing {missing}")
            return None
    if not any(entry.get("default") for entry in models.values()):
        _log(f"catalogue {path} names no default model for {backend}")
        return None
    return models


def _load_catalog(backend: str) -> dict:
    for path in _catalog_paths():
        if not path.exists():
            continue
        models = _read_catalog(path, backend)
        if models is not None:
            _log(f"catalogue: {len(models)} {backend} model(s) from {path}")
            return models
    raise RuntimeError("no usable catalogue found; the bundled copy is missing")



_models: dict[str, object] = {}


def _backend() -> str:
    """Which inference stack this interpreter carries.

    Decided by what is importable rather than by platform: the MLX pack has
    mlx_speech, the torch pack has dots_tts, and the same engine.py serves
    both. find_spec keeps startup cheap — neither stack is imported until a
    model loads. A machine with neither answers "mlx" so the existing failure
    message at first load names what is missing.
    """
    from importlib.util import find_spec

    if find_spec("mlx_speech") is not None:
        return "mlx"
    if find_spec("dots_tts") is not None:
        return "torch"
    return "mlx"


BACKEND = _backend()
MODELS = _load_catalog(BACKEND)


def _default_model() -> str:
    return next(k for k, v in MODELS.items() if v["default"])


def _load(model_id: str):
    if model_id not in MODELS:
        raise ValueError(f"unknown model {model_id!r}; have {sorted(MODELS)}")
    if model_id not in _models and not _is_installed(MODELS[model_id]):
        raise ValueError(f"model {model_id!r} is not installed")
    if model_id not in _models:
        started = time.perf_counter()
        if BACKEND == "torch":
            import dots_torch

            _models[model_id] = dots_torch.load(MODELS[model_id])
        else:
            from mlx_speech import tts

            _models[model_id] = tts.load(MODELS[model_id]["alias"])
        elapsed = time.perf_counter() - started
        _remember_load_time(model_id, elapsed)
        _log(f"loaded {model_id} in {elapsed:.1f}s")
    return _models[model_id]


def _load_times() -> dict[str, float]:
    try:
        return json.loads(_LOAD_TIMES.read_text())
    except Exception:
        return {}


def _remember_load_time(model_id: str, seconds: float) -> None:
    """Keep the last measured load, so "loads in 8s" is this machine's 8s."""
    times = _load_times()
    times[model_id] = round(seconds, 1)
    try:
        _LOAD_TIMES.parent.mkdir(parents=True, exist_ok=True)
        _LOAD_TIMES.write_text(json.dumps(times, indent=2))
    except OSError as exc:
        _log(f"could not record load time: {exc}")




# ----------------------------------------------------------------- methods
def m_list_models(params: dict) -> dict:
    # `refresh` is the deliberate online step, asked for by the model screen.
    sizes = _download_sizes(refresh=bool(params.get("refresh")))
    load_times = _load_times()
    models = []
    for key, spec in MODELS.items():
        entry = {f: v for f, v in spec.items() if f != "gen"}
        entry["id"] = key
        entry["installed"] = _is_installed(spec)
        entry["size_bytes"] = _cached_size(spec) if entry["installed"] else 0
        # What the download will cost, distinct from what is already on disk.
        entry["download_bytes"] = sizes.get(key, 0)
        sub = spec.get("subfolder") or ""
        entry["precision"] = spec.get("precision") or (
            "int8 quantised" if "int8" in sub else "full precision" if "base" in sub else "8-bit"
        )
        # Resident right now, versus on disk and needing a load first.
        entry["resident"] = key in _models
        entry["load_s"] = load_times.get(key)
        models.append(entry)
    return {"models": models}


def m_load_model(params: dict) -> dict:
    model_id = params["model"]
    started = time.perf_counter()
    _load(model_id)
    return {"model": model_id, "load_s": round(time.perf_counter() - started, 2)}


def _trim_silence(path: Path) -> tuple[float, float]:
    """Cut leading and trailing silence off a reference recording, in place.

    A recording keeps whatever surrounded the words — the pause after pressing
    record, the wait before stop. The model conditions on the reference as a
    continuation, so a clone inherits that dead air as its opening; and the
    speaker embedding reads only the first ten seconds, so a long lead-in
    spends identity on room tone.

    Two gates, and whichever sits higher decides: 30 dB under the recording's
    own peak (upstream's prompt-trim figure), or an absolute -38 dBFS. The
    absolute gate is what makes this work on quiet takes — a real lead-in
    measured here sat 22 dB under a -25 dBFS peak, invisible to any relative
    gate, while nothing the recorder accepts as speech sits below -38 dBFS.
    150 ms of padding stays on either side, and a file the trim would erase is
    left untouched.
    """
    wav, sr = sf.read(path, always_2d=True)
    window = max(1, int(0.02 * sr))
    frames = len(wav) // window
    if frames == 0:
        return 0.0, 0.0
    mono = np.mean(np.abs(wav[: frames * window]), axis=1)
    rms = np.sqrt(np.mean(mono.reshape(frames, window) ** 2, axis=1))
    relative = float(rms.max()) * 10 ** (-30 / 20)
    floor = 10 ** (-38 / 20)
    loud = np.flatnonzero(rms > max(relative, floor))
    if loud.size == 0:
        return 0.0, 0.0
    pad = int(0.15 * sr)
    start = max(0, int(loud[0]) * window - pad)
    end = min(len(wav), (int(loud[-1]) + 1) * window + pad)
    lead, tail = start / sr, (len(wav) - end) / sr
    if lead < 0.05 and tail < 0.05:
        return 0.0, 0.0
    sf.write(path, wav[start:end], sr)
    return round(lead, 2), round(tail, 2)


def _condition(
    reference_audio: str, reference_text: str | None, model_id: str | None = None
) -> float:
    """Materialise speaker conditioning so later generations skip that cost.

    Takes the recording itself rather than an identifier to look up. Which
    voices exist is the application's to know, and an engine that had to be told
    about a voice before it could speak with it would be keeping a second
    register of them.
    """
    model_id = model_id or _default_model()
    model = _load(model_id)
    gen = MODELS[model_id].get("gen") or {}

    started = time.perf_counter()
    prepare = getattr(model, "prepare_prompt", None)
    if prepare is not None:
        # The direct path: no waveform is synthesised, only the conditioning.
        prepare(
            reference_audio,
            reference_text=reference_text or None,
            speaker_scale=gen.get("speaker_scale", 1.5),
        )
    else:
        # Backends without prepare_prompt warm the same cache by generating.
        kwargs = dict(gen)
        kwargs["reference_audio"] = reference_audio
        if reference_text:
            kwargs["reference_text"] = reference_text
        model.generate("Ready.", **kwargs)
    return round(time.perf_counter() - started, 2)


class UnsupportedCacheLayout(Exception):
    """A model is loaded but its conditioning cache cannot be located.

    Distinct from an empty cache: that one means there is nothing to remove,
    which is a normal result. This one means derived conditioning may still be
    reachable by the engine and this process can no longer prove otherwise, so
    the only honest answer is to end the process holding it.
    """


@dataclass(frozen=True)
class ConditioningInvalidation:
    entries_removed: int
    # The cache is keyed on the waveform, so there is no per-voice key to evict
    # from outside and one deletion clears every voice. Reported rather than
    # implied: the caller must not read this as a targeted eviction.
    scope_applied: str
    status: str

    def as_reply(self) -> dict:
        return {
            "entries_removed": self.entries_removed,
            "scope_applied": self.scope_applied,
            "status": self.status,
        }


# Where each supported backend keeps derived conditioning. Named rather than
# searched for: the first version walked the model object looking for anything
# clearable, found nothing on an adapter that keeps its cache one level in, and
# reported success. A lookup that does not know where to look cannot tell that
# apart from a cache that is already empty.
_CACHE_LOCATIONS = (
    ("_generator", "_prompt_cache", "_prompt_cache_lock"),
    (None, "_prompt_cache", "_prompt_cache_lock"),
    (None, "prompt_cache", None),
)


def _conditioning_cache(model: object) -> tuple[object, object | None]:
    """The mapping holding derived conditioning, and whatever guards it."""
    for inner, cache_name, lock_name in _CACHE_LOCATIONS:
        holder = getattr(model, inner, None) if inner else model
        if holder is None:
            continue
        cache = getattr(holder, cache_name, None)
        if cache is None or not hasattr(cache, "clear"):
            continue
        return cache, getattr(holder, lock_name, None) if lock_name else None
    raise UnsupportedCacheLayout(
        f"no conditioning cache found on {type(model).__name__}"
    )


def _forget_conditioning() -> ConditioningInvalidation:
    """Drop the speaker conditioning held in memory by every loaded model.

    A deleted voice leaves its recording on disk gone, but the embedding and
    acoustic prompt derived from it stay resident until something removes them
    — which is still the person's voice, in memory, after they asked for it to
    be removed.

    This is eviction, not erasure: the allocator may hold the freed pages, and
    nothing here can promise otherwise. What it does promise is that the engine
    has no reachable conditioning left to speak with.
    """
    removed = 0
    for model in _models.values():
        cache, lock = _conditioning_cache(model)
        with lock if lock is not None else contextlib.nullcontext():
            removed += len(cache) if hasattr(cache, "__len__") else 0
            cache.clear()
    result = ConditioningInvalidation(
        entries_removed=removed,
        scope_applied="all",
        status="cleared" if removed else "already_empty",
    )
    _log(f"conditioning {result.status}: {removed} entr(ies), scope {result.scope_applied}")
    return result


MAX_AUDIO_PATCHES = 500
PATCHES_PER_SECOND = 6.25
WORDS_PER_SECOND = 3.2
# Target well under the ceiling: the estimate is rough, and running into the cap
# mid-sentence is far worse than using one extra chunk.
#
# 40 was tried and reverted. The measurement that suggested it — 0.866 speaker
# similarity at 110 words against 0.985 at 40 — compared a clip generated with
# the model's *default* voice against one generated with the user's, so it
# measured the wrong thing entirely. Controlled properly, same voice and text
# and seed: 110 words scores 0.985/0.980/0.985/0.980 and 40 scores 0.985 four
# times, which is the same answer, and 40 takes 162s where 110 takes 127s.
CHUNK_TARGET_WORDS = 110

# When even a calibrated chunk cannot be made big enough to be worth speaking,
# the reference is eating nearly the whole budget.
CHUNK_FLOOR_WORDS = 20
# The model's pace on long text is not the reference's reading pace; the gap
# measured here was up to thirty percent, and running out of budget cuts a
# sentence off mid-word.
PACE_SAFETY = 0.7


def _calibrated_target_words(
    patch_seconds: float | None,
    budget_patches: int,
    reference_seconds: float,
    reference_words: int,
) -> int:
    """How many words fit one generation, for this voice and this reference.

    The model has one budget for reference and output together, so a longer
    reference means less room to speak — a 42 second reference was measured
    leaving 37 of 80 seconds, and a fixed 110-word chunk then stops mid-word at
    the ceiling with nothing reporting it. The voice's own pace comes from the
    reference itself: its duration over its words is how fast this person
    talks, which is the number a fixed target in words has to guess.
    """
    if not patch_seconds or budget_patches <= 0:
        return CHUNK_TARGET_WORDS
    reference_patches = (
        max(0, math.ceil(reference_seconds / patch_seconds) - 1)
        if reference_seconds > 0
        else 0
    )
    speakable = (budget_patches - reference_patches - 2) * patch_seconds
    if speakable <= 0:
        return CHUNK_FLOOR_WORDS
    pace = (
        reference_seconds / reference_words
        if reference_seconds > 0 and reference_words > 0
        # Slower than anyone measured here, because the cost of guessing slow
        # is smaller chunks and the cost of guessing fast is a cut sentence.
        else 0.55
    )
    return max(CHUNK_FLOOR_WORDS, min(CHUNK_TARGET_WORDS, int(speakable * PACE_SAFETY / pace)))


def _patch_seconds(model) -> float | None:
    """One patch of output, in seconds, where the backend has such a thing."""
    try:
        config = model._generator.config
        return config.patch_size * config.vocoder.hop_size / config.vocoder.sample_rate
    except AttributeError:
        return None


def _split_into_chunks(text: str, target_words: int = CHUNK_TARGET_WORDS) -> list[str]:
    """Split on sentence boundaries, packing sentences up to the target size.

    Splitting mid-sentence would cut prosody in an audible place, so a single
    sentence longer than the target is left whole and allowed to be oversized.
    """
    sentences = [s.strip() for s in re.split(r"(?<=[.!?])\s+", text.strip()) if s.strip()]
    if not sentences:
        return []

    chunks: list[str] = []
    current: list[str] = []
    count = 0
    for sentence in sentences:
        words = len(sentence.split())
        if current and count + words > target_words:
            chunks.append(" ".join(current))
            current, count = [], 0
        current.append(sentence)
        count += words
    if current:
        chunks.append(" ".join(current))
    return chunks


# --- model installation -----------------------------------------------------
# Weights are downloaded rather than bundled: they are gigabytes, they update
# independently of the app, and a user who only wants one model should not pay
# for five. Progress is polled rather than pushed, because the protocol is one
# response per request and a long-running download does not fit that shape.

_installs: dict[str, dict] = {}
_installs_lock = threading.Lock()


def _offline_by_default() -> None:
    """Forbid network access except while a download is explicitly running.

    The privacy claim is that a voice never leaves the machine. Verifying that
    once is weaker than making it structural: with the hub pinned offline,
    generation cannot reach the network even by accident — a stray cache lookup
    fails loudly instead of quietly fetching.
    """
    os.environ.setdefault("HF_HUB_OFFLINE", "1")


class _online:
    """Lift the offline pin for the duration of a deliberate download.

    The environment variable alone is not enough. `huggingface_hub` reads
    `HF_HUB_OFFLINE` once, at import, into `constants.HF_HUB_OFFLINE`, and every
    request checks `constants.is_offline_mode()` — which returns that latched
    value. Setting the variable after import changes nothing, so the flag itself
    has to be flipped, and put back afterwards so the pin still holds.
    """

    def __enter__(self):
        from huggingface_hub import constants

        self._constants = constants
        self._previous_env = os.environ.get("HF_HUB_OFFLINE")
        self._previous_flag = constants.HF_HUB_OFFLINE
        os.environ["HF_HUB_OFFLINE"] = "0"
        constants.HF_HUB_OFFLINE = False
        return self

    def __exit__(self, *_):
        self._constants.HF_HUB_OFFLINE = self._previous_flag
        if self._previous_env is None:
            os.environ.pop("HF_HUB_OFFLINE", None)
        else:
            os.environ["HF_HUB_OFFLINE"] = self._previous_env
        return False


def _model_patterns(spec: dict) -> list[str] | None:
    """Restrict a download to one variant when the repo holds several."""
    sub = spec.get("subfolder")
    return [f"{sub}/*"] if sub else None


def _cached_size(spec: dict) -> int:
    """Bytes already present in the local cache for this model."""
    from huggingface_hub import snapshot_download

    try:
        path = Path(
            snapshot_download(
                spec["repo"],
                revision=spec.get("revision"),
                allow_patterns=_model_patterns(spec),
                local_files_only=True,
            )
        )
    except Exception:
        return 0
    root = path / spec["subfolder"] if spec.get("subfolder") else path
    if not root.exists():
        return 0
    return sum(f.stat().st_size for f in root.rglob("*") if f.is_file())


def _remote_size(spec: dict) -> int:
    """Total download size, from the hub rather than guessed."""
    from huggingface_hub import HfApi

    try:
        with _online():
            info = HfApi().model_info(
                spec["repo"], revision=spec.get("revision"), files_metadata=True
            )
    except Exception:
        return 0
    sub = spec.get("subfolder")
    total = 0
    for sibling in info.siblings or []:
        if sub and not sibling.rfilename.startswith(f"{sub}/"):
            continue
        total += sibling.size or 0
    return total


# Download sizes come from the hub, never from a guess, so the number the user
# commits disk to is the number that will be written. The look-up needs network,
# so the answer is kept on disk: after one online visit the size is still shown
# to an offline user, and a machine that has never been online shows none rather
# than an invented figure.
_SIZE_CACHE = VOICE_DIR.parent / "model-sizes.json"
# The protocol's, not a second one: a stop is the same fact however it was
# asked for, and the serving layer recognises this class specifically.
Cancelled = protocol.Cancelled


_LOAD_TIMES = VOICE_DIR.parent / "model-load-times.json"


def _download_sizes(refresh: bool = False) -> dict[str, int]:
    try:
        known = json.loads(_SIZE_CACHE.read_text())
    except Exception:
        known = {}
    if not refresh:
        return known
    for key, spec in MODELS.items():
        size = _remote_size(spec)
        if size:
            known[key] = size
    try:
        _SIZE_CACHE.parent.mkdir(parents=True, exist_ok=True)
        _SIZE_CACHE.write_text(json.dumps(known, indent=2))
    except OSError as exc:
        _log(f"could not cache model sizes: {exc}")
    return known


def _is_installed(spec: dict) -> bool:
    # A partial cache is not an install; require most of the expected bytes.
    remote = spec.get("_remote_size") or 0
    cached = _cached_size(spec)
    if cached == 0:
        return False
    return remote == 0 or cached >= remote * 0.98


def _download(model_id: str) -> None:
    from huggingface_hub import snapshot_download

    spec = MODELS[model_id]

    def progress() -> None:
        """Poll the cache directory; hub callbacks are not part of the API."""
        while True:
            with _installs_lock:
                state = _installs.get(model_id)
                if state is None or state["state"] != "downloading":
                    return
                state["downloaded_bytes"] = _cached_size(spec)
            time.sleep(1.0)

    watcher = threading.Thread(target=progress, daemon=True)
    watcher.start()
    try:
        with _online():
            snapshot_download(
                spec["repo"],
                revision=spec.get("revision"),
                allow_patterns=_model_patterns(spec),
            )
        with _installs_lock:
            _installs[model_id].update(state="installed", downloaded_bytes=_cached_size(spec))
        _log(f"installed {model_id}")
    except Exception as exc:
        with _installs_lock:
            _installs[model_id].update(state="failed", error=f"{type(exc).__name__}: {exc}")
        _log(f"install failed for {model_id}: {exc}")


def m_install_model(params: dict) -> dict:
    model_id = params["model"]
    if model_id not in MODELS:
        raise ValueError(f"unknown model {model_id!r}")

    with _installs_lock:
        current = _installs.get(model_id)
        if current and current["state"] == "downloading":
            return {"model": model_id, **current}
        _installs[model_id] = {
            "state": "downloading",
            "downloaded_bytes": 0,
            "total_bytes": _remote_size(MODELS[model_id]),
            "error": None,
        }

    threading.Thread(target=_download, args=(model_id,), daemon=True).start()
    with _installs_lock:
        return {"model": model_id, **_installs[model_id]}


def m_delete_model(params: dict) -> dict:
    """Remove a model's weights from the cache and drop it from memory.

    Deletes only this model's subfolder when the repo holds several variants,
    so removing "Fast" cannot take "Best quality" with it.
    """
    from huggingface_hub import snapshot_download

    model_id = params["model"]
    if model_id not in MODELS:
        raise ValueError(f"unknown model {model_id!r}")
    spec = MODELS[model_id]
    _models.pop(model_id, None)

    try:
        root = Path(
            snapshot_download(
                spec["repo"],
                revision=spec.get("revision"),
                allow_patterns=_model_patterns(spec),
                local_files_only=True,
            )
        )
    except Exception:
        return {"model": model_id, "freed_bytes": 0}

    target = root / spec["subfolder"] if spec.get("subfolder") else root
    freed = 0
    if target.exists():
        for file in target.rglob("*"):
            if file.is_file():
                # Cached files are symlinks into blobs/; the bytes live there.
                blob = file.resolve()
                freed += blob.stat().st_size if blob.exists() else 0
                blob.unlink(missing_ok=True)
                file.unlink(missing_ok=True)
        shutil.rmtree(target, ignore_errors=True)
    _log(f"deleted {model_id}, freed {freed / 1e9:.2f} GB")
    return {"model": model_id, "freed_bytes": freed}


def m_install_status(params: dict) -> dict:
    model_id = params["model"]
    with _installs_lock:
        state = _installs.get(model_id)
        if state is not None:
            return {"model": model_id, **state}
    spec = MODELS[model_id]
    installed = _is_installed(spec)
    return {
        "model": model_id,
        "state": "installed" if installed else "absent",
        "downloaded_bytes": _cached_size(spec),
        "total_bytes": spec.get("_remote_size") or 0,
        "error": None,
    }


def m_system_info(_params: dict) -> dict:
    """What this machine is, read from it rather than assumed."""
    import subprocess

    def sysctl(key: str) -> str:
        try:
            return subprocess.check_output(["sysctl", "-n", key], text=True).strip()
        except Exception:
            return ""

    memory_bytes = 0
    chip = ""
    if platform.system() == "Darwin":
        chip = sysctl("machdep.cpu.brand_string")
        try:
            memory_bytes = int(sysctl("hw.memsize") or 0)
        except ValueError:
            memory_bytes = 0

    usage = shutil.disk_usage(str(Path.home()))
    return {
        # mac_ver reports the macOS version; platform.system() says "Darwin",
        # which is the kernel and not what anyone calls their Mac.
        "os": (
            f"macOS {platform.mac_ver()[0]}"
            if platform.system() == "Darwin" and platform.mac_ver()[0]
            else f"{platform.system()} {platform.release()}"
        ),
        "chip": chip or platform.machine(),
        "memory_bytes": memory_bytes,
        "free_bytes": usage.free,
        # The whole disk as well as what is left, so one answer about this
        # machine covers what the application needs to know about it.
        "total_bytes": usage.total,
        "data_dir": str(VOICE_DIR.parent),
    }


# Below this, a take is mostly fixed cost — warming caches, the first pass
# through the model — and its rate says nothing about how long a real clip will
# take. One such take was 1.4 seconds of audio in 416 seconds.
RATE_SAMPLE_MIN_SECONDS = 3.0


def m_ping(_params: dict) -> dict:
    return {"pong": True}


def _capabilities() -> dict:
    """What this engine is, stated once at the handshake."""
    return {
        "backend": BACKEND,
        "models": sorted(MODELS),
        "default_model": _default_model(),
        # The cache is keyed on the waveform, so forgetting one voice forgets
        # them all. Said here rather than discovered when a deletion reports it.
        "conditioning_eviction": "all",
    }


def m_conditioning_invalidate(_params: dict, _ctx: protocol.Context) -> dict:
    """Forget every voice this engine has derived conditioning for."""
    return _forget_conditioning().as_reply()


def m_conditioning_prepare(params: dict, _ctx: protocol.Context) -> dict:
    """Warm the conditioning for a recording the caller supplies."""
    return {
        "prepared_s": _condition(
            _recording(params["reference_audio"]),
            params.get("reference_text"),
            params.get("model"),
        )
    }


def _recording(path: str) -> str:
    """A reference recording that is actually there.

    Checked before the model is asked for it so a deleted voice is a plain
    answer rather than whatever the backend does with a missing file.
    """
    if not Path(path).exists():
        raise FileNotFoundError(f"no recording at {path}")
    return path


class _Reporting:
    """Keep saying where a generation has got to while a chunk is running.

    A chunk is one blocking call into the model, so the loop below can only
    report at chunk boundaries — and a short clip is a single chunk, which left
    the application showing nothing at all for the whole wait and then
    finishing. This says the same fields between boundaries, advancing only the
    one it actually knows: the clock. `written_s` stays where the last finished
    chunk put it, because nothing has been written since.

    Cheap to do often: the writer collapses progress per execution, so a
    subscriber that is behind sees the latest rather than all of them.
    """

    def __init__(self, ctx: protocol.Context, total: int, period: float = 0.25) -> None:
        self._ctx = ctx
        self._total = total
        self._period = period
        self.written_s = 0.0
        self.done = 0
        self._started = time.perf_counter()
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def _say(self) -> None:
        self._ctx.emit(
            "job.progress",
            {
                "chunks_done": self.done,
                "chunks": self._total,
                "written_s": round(self.written_s, 2),
                "elapsed_s": round(time.perf_counter() - self._started, 2),
            },
        )

    def _run(self) -> None:
        while not self._stop.wait(self._period):
            self._say()

    def __enter__(self) -> "_Reporting":
        self._say()
        self._thread.start()
        return self

    def chunk_done(self, written_s: float, done: int) -> None:
        self.written_s = written_s
        self.done = done
        self._say()

    def __exit__(self, *_exc: object) -> None:
        self._stop.set()
        self._thread.join(timeout=1.0)


# Small on purpose. This is not transcription for a person to read — it only
# has to find where a reader stopped inside a script we already have, and the
# base model does that in under a second. Measured against this recording:
# `small` reached the same answer seventeen times slower, and `tiny` overshot
# the ending, which is the one direction that is not safe.
ASR_MODEL = "mlx-community/whisper-base-mlx"


def _listen_back(audio: Path, script: str | None) -> tuple[str, float] | None:
    """What the recording says, and how much of it to keep.

    The reference text and the reference audio have to describe each other. The
    model generates the rest of the reference and then what it was asked for,
    so the boundary between the two is wherever the text and the audio stop
    agreeing. Told about words the audio lacks, it speaks them first — the
    defect this exists for. Given audio the text does not cover, it spends the
    opening of the requested line accounting for it, and that line comes back
    with its first words missing. Neither direction is safe.

    So both are cut, to the last sentence the reader actually finished. A
    sentence boundary rather than the last word recognised, because a reference
    ending mid-clause leaves the model a clause to close and it closes it with
    the words it was asked to say. Measured on both: cutting text alone,
    cutting audio alone, and cutting either mid-clause each broke one sentence
    or the other; cutting both at a sentence boundary broke neither.

    Returns the text and the seconds of audio it describes, or `None` when
    nothing can be established — which is not an empty reference but no
    opinion, leaving the caller with what it already believed.
    """
    try:
        import mlx_whisper
    except ImportError:
        # A runtime without speech recognition. Older ones had none, and the
        # protocol has to keep working for them.
        return None

    try:
        listened = mlx_whisper.transcribe(
            str(audio), path_or_hf_repo=ASR_MODEL, language="en", word_timestamps=True
        )
    except Exception as failure:  # noqa: BLE001 - never fail enrolment over this
        _log(f"could not listen back to the reference: {failure}")
        return None

    heard = [
        (_words(word["word"]), word["start"], word["end"])
        for segment in listened.get("segments", [])
        for word in segment.get("words", [])
    ]
    heard = [(w[0], start, end) for w, start, end in heard if w]
    if not heard or not script or not script.strip():
        return None
    return _up_to_the_last_finished_sentence(script, heard)


def _words(text: str) -> list[str]:
    return re.findall(r"[a-z0-9']+", text.lower())


def _up_to_the_last_finished_sentence(
    script: str, heard: list[tuple[str, float, float]]
) -> tuple[str, float] | None:
    """Align what was heard to the script, then back off to a sentence end.

    Aligned as a subsequence rather than word by word: recognition mishears,
    and a walk needing each next word exactly stops at the first mistake, which
    reports a fluent reader as having said almost nothing.
    """
    script_words = _words(script)
    if not script_words:
        return None

    blocks = [
        b
        for b in difflib.SequenceMatcher(
            None, script_words, [w for w, _, _ in heard], autojunk=False
        ).get_matching_blocks()
        if b.size
    ]
    if not blocks:
        # Nothing lined up: a bad recording, another language, another script.
        # Guessing between those would be worse than having no opinion.
        return None
    reached = max(b.a + b.size for b in blocks)

    # The sentences the reader got all the way through.
    finished = [
        (mark.end(), len(_words(script[: mark.end()])))
        for mark in re.finditer(r"[.!?]", script)
    ]
    finished = [(chars, words) for chars, words in finished if words <= reached]
    if not finished:
        # Not one sentence completed. There is no clean pair to cut to, and a
        # reference this short was not going to clone a voice anyway.
        return None

    cut_chars, cut_words = finished[-1]

    # Through the alignment, not by position: recognition inserts and drops
    # words, so the nth word of the script is not the nth word of what was
    # heard, and taking it by index kept a second of the following sentence.
    #
    # Cut where the next word begins rather than where the last one ended. The
    # words at a sentence's end are exactly the ones recognition is likeliest
    # to have missed — "calm voice" came back as "comfort" — and cutting at the
    # last word it did catch stops the audio before the sentence is finished,
    # which leaves the model a word of the reference to say first.
    at_or_after = [
        heard[block.b + (want - block.a)][1]
        for want in range(cut_words, len(script_words))
        for block in blocks
        if block.a <= want < block.a + block.size
    ]
    if at_or_after:
        return script[:cut_chars].strip(), at_or_after[0]

    # Nothing was recognised after this sentence, so the recording ends with
    # it. Keep all of it.
    return script[:cut_chars].strip(), heard[-1][2] + 0.4


def m_audio_prepare_reference(params: dict, _ctx: protocol.Context) -> dict:
    """Make a recording usable as a reference, at the path the caller names.

    Waveform work, which is why it is here: cutting the silence off a take and
    measuring what is left needs the audio stack, and nothing else in the
    application has one. Where the file goes and what it then means are the
    caller's — this writes where it is told and reports what it wrote.

    It also listens back. `script` is what the caller believes was read; what
    comes back in `text` is as much of it as the recording supports.
    """
    source = Path(params["source"])
    if not source.exists():
        raise FileNotFoundError(str(source))
    out = Path(params["output_path"])
    out.parent.mkdir(parents=True, exist_ok=True)
    if source.resolve() != out.resolve():
        shutil.copy2(source, out)

    lead, tail = _trim_silence(out)
    if lead or tail:
        _log(f"trimmed {lead}s lead-in and {tail}s tail from {out.name}")
    info = sf.info(out)

    # After trimming, so what is listened to is what will be the reference.
    listened = _listen_back(out, params.get("script"))
    text = None
    if listened is not None:
        text, keep_until = listened
        if keep_until < info.frames / info.samplerate:
            audio, rate = sf.read(out, dtype="float32")
            sf.write(out, audio[: int(keep_until * rate)], rate)
            info = sf.info(out)
            _log(
                f"the reader stopped early: keeping {info.frames / info.samplerate:.1f}s "
                f"and {len(_words(text))} of {len(_words(params['script']))} script words"
            )

    prepared = {
        "output_path": str(out),
        "seconds": round(info.frames / info.samplerate, 1),
        "trimmed_lead_s": lead,
        "trimmed_tail_s": tail,
    }
    if text is not None:
        prepared["text"] = text
    return prepared


# The model starts speaking at the very first sample, and everything that
# plays audio — DACs ramping up, Bluetooth waking, a transcriber deciding where
# speech begins — eats an abrupt onset. A five-word clip audibly lost its "The"
# to this; the word was in the file the whole time.
LEAD_IN_S = 0.2

# A pause longer than anyone leaves mid-sentence. The reference this was tuned
# against pauses for 1.9s at its most deliberate; SOAR was measured leaving
# seven seconds of nothing in a forty-word clip, which is a defect, not a
# breath.
DEAD_AIR_S = 2.0
KEPT_PAUSE_S = 0.8
TAIL_S = 0.4


def _settle_edges_and_pauses(wav, sample_rate: int):
    """Give the clip a lead-in, collapse dead air, and trim a dragging tail.

    Pauses up to DEAD_AIR_S are delivery and are not touched. Beyond that they
    are collapsed to KEPT_PAUSE_S rather than removed: the sentence around them
    still needs its breath, it does not need the room to go silent.
    """
    window = max(1, int(0.05 * sample_rate))
    frames = len(wav) // window
    if frames == 0:
        return wav
    rms = np.sqrt(
        (wav[: frames * window].reshape(frames, window) ** 2).mean(axis=1)
    )
    quiet = rms < max(1e-4, 0.03 * float(rms.max()))

    keep = np.ones(len(wav), dtype=bool)
    run_start = None
    for index in range(frames + 1):
        if index < frames and quiet[index]:
            if run_start is None:
                run_start = index
            continue
        if run_start is not None:
            run_frames = index - run_start
            if run_start == 0:
                # The opening is not a pause. Whatever silence the model put
                # before its first word says nothing — the lead-in added below
                # is the onset, so none of this is worth keeping.
                limit, kept = 0.0, 0.0
            elif index == frames:
                limit, kept = TAIL_S, TAIL_S
            else:
                limit, kept = DEAD_AIR_S, KEPT_PAUSE_S
            if run_frames * 0.05 > limit:
                cut_from = run_start * window + int(kept * sample_rate)
                cut_to = index * window if index < frames else len(wav)
                keep[cut_from:cut_to] = False
            run_start = None
    settled = wav[keep]

    lead = np.zeros(int(LEAD_IN_S * sample_rate), dtype=wav.dtype)
    return np.concatenate([lead, settled])


def m_synthesis_generate(params: dict, ctx: protocol.Context) -> dict:
    """Speak the text into the file the caller named, and record nothing.

    One file, at the path it was given. Whether that audio is kept, where it
    belongs, and what it becomes are decided after someone has looked at it —
    an engine that filed its own output would be settling that here, before
    anything had checked the result.
    """
    model_id = params.get("model") or _default_model()
    model = _load(model_id)

    kwargs = dict(MODELS[model_id].get("gen") or {})
    kwargs.update(params.get("options") or {})
    # Random unless the caller pins one, so "generate again" gives a different
    # take. The seed used is returned, which is what makes a take repeatable.
    seed = params.get("seed")
    if seed is None:
        seed = random.randint(1000, 9999)
    kwargs["seed"] = int(seed)
    # Absent means the model's own voice. There is no identifier to look up:
    # which voices exist is the application's to know.
    if params.get("reference_audio"):
        kwargs["reference_audio"] = _recording(params["reference_audio"])
        if params.get("reference_text"):
            kwargs["reference_text"] = params["reference_text"]

    # Sized for this voice: the reference spends part of the model's one
    # budget, and the reference's own duration over its own words is how fast
    # this person speaks.
    reference_seconds = 0.0
    reference_words = 0
    if params.get("reference_audio"):
        try:
            info = sf.info(_recording(params["reference_audio"]))
            reference_seconds = info.frames / info.samplerate
        except Exception:  # noqa: BLE001 - sizing falls back, generation decides
            reference_seconds = 0.0
        reference_words = len(_words(params.get("reference_text") or ""))
    target_words = _calibrated_target_words(
        _patch_seconds(model),
        int(kwargs.get("max_audio_patches") or 0),
        reference_seconds,
        reference_words,
    )
    chunks = _split_into_chunks(params["text"], target_words)
    if not chunks:
        raise ValueError("nothing to say")
    if target_words != CHUNK_TARGET_WORDS:
        _log(
            f"chunking for this voice: {target_words} words per chunk "
            f"({reference_seconds:.0f}s reference)"
        )
    out = Path(params["output_path"])

    started = time.perf_counter()
    pieces = []
    sample_rate = None
    written_s = 0.0
    with _Reporting(ctx, len(chunks)) as reporting:
        for index, chunk in enumerate(chunks):
            # Between chunks rather than mid-utterance: a chunk boundary is a
            # sentence end, and stopping inside one leaves half a sentence.
            if ctx.cancelled():
                raise protocol.Cancelled(f"stopped before chunk {index + 1} of {len(chunks)}")
            result = model.generate(chunk, **kwargs)
            sample_rate = result.sample_rate
            piece = np.asarray(result.waveform, dtype=np.float32).squeeze()
            pieces.append(piece)
            written_s += piece.shape[0] / sample_rate
            reporting.chunk_done(written_s, index + 1)
            if index + 1 < len(chunks):
                # A short gap between chunks reads as a breath rather than a join.
                pieces.append(np.zeros(int(0.18 * sample_rate), dtype=np.float32))
    gen_s = time.perf_counter() - started

    wav = np.concatenate(pieces) if len(pieces) > 1 else pieces[0]
    wav = _settle_edges_and_pauses(wav, sample_rate)
    # Normalise once over the whole utterance: per-chunk normalisation would
    # make the volume step at every join.
    peak = float(np.max(np.abs(wav)))
    if peak > 0:
        wav = wav * (0.95 / peak)

    out.parent.mkdir(parents=True, exist_ok=True)
    sf.write(out, wav, sample_rate)
    audio_s = wav.shape[0] / sample_rate
    return {
        "output_path": str(out),
        "model": model_id,
        "audio_s": round(audio_s, 2),
        "gen_s": round(gen_s, 2),
        "rtf": round(gen_s / audio_s, 2) if audio_s else None,
        "seed": int(seed),
        "sample_rate": sample_rate,
        "chunks": len(chunks),
    }


def _plain(handler) -> protocol.Handler:
    """An older handler, which takes parameters and nothing else."""
    return lambda params, _ctx: handler(params)


# Answered on the reader, because none of these touch the model: they stay
# available while it is loading a model or speaking for a minute.
#
# Everything the engine can be asked is one of these two lists. There is nothing
# here about clips, voices or consent — those are the application's, kept in its
# database, and a method that answered for them would make this process a second
# place they live: the one the application would then have to agree with.
JSONRPC_BROKER: dict[str, protocol.BrokerHandler] = {
    "ping": m_ping,
    "system_info": m_system_info,
    "model.install_status": m_install_status,
}

# Queued on the thread that owns the model. Sorted by what they touch rather
# than by how long they take: a fast call that reads the model still has to
# wait for the slow one that is writing it.
JSONRPC_MODEL: dict[str, protocol.Handler] = {
    "model.list": _plain(m_list_models),
    "model.load": _plain(m_load_model),
    "model.install": _plain(m_install_model),
    "model.delete": _plain(m_delete_model),
    "audio.prepare_reference": m_audio_prepare_reference,
    "conditioning.prepare": m_conditioning_prepare,
    "conditioning.invalidate": m_conditioning_invalidate,
    "synthesis.generate": m_synthesis_generate,
}

def main() -> None:
    _offline_by_default()
    _log("sidecar ready")
    protocol.serve(
        broker=JSONRPC_BROKER, model=JSONRPC_MODEL, capabilities=_capabilities()
    )


if __name__ == "__main__":
    main()
