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

import json
import os
import shutil
import sys
import time
import traceback
from pathlib import Path

import platform
import random
import re
import threading

import numpy as np
import soundfile as sf

# Generation settings for the dots family, validated across 8 Nigerian speakers:
# 24/24 identity separation, median 0.0% WER. Do not change without re-running
# the sanity harness in voice-clone-bench.
DOTS_GEN = {
    "guidance_scale": 1.2,
    "speaker_scale": 1.5,
    "max_audio_patches": 500,
    "eos_threshold": 0.8,
    "template": "tts",
}

# Catalogue of models the app may offer. `label` is what this app calls it;
# `name` is what it actually is, which is the one that means anything to
# someone checking a licence. Every entry must be commercially
# licensed — models under non-commercial terms are deliberately absent, which
# is why Fish S2 Pro (research-only) and F5-TTS (CC-BY-NC weights) are missing
# despite the runtime being able to load them.
#
# `gen` carries per-model settings rather than one shared dict: the adapter
# layer silently drops kwargs a backend does not use, and silence is a poor
# place to discover that a setting never applied.
MODELS = {
    "dots-tts-mf": {
        "label": "Fast",
        "name": "dots.tts MF",
        "alias": "dots-tts-mf",
        "repo": "appautomaton/dots-tts-mlx",
        "subfolder": "mf/mlx-int8",
        "licence": "Apache-2.0",
        "default": True,
        "notes": "Validated default. Best measured accent retention, ~3.5x faster than SOAR.",
        "supports_cloning": True,
        "gen": DOTS_GEN,
    },
    "dots-tts-mf-base": {
        "label": "Fast, full precision",
        "name": "dots.tts MF",
        "alias": "dots-tts-mf-base",
        "repo": "appautomaton/dots-tts-mlx",
        "subfolder": "mf/mlx-base",
        "licence": "Apache-2.0",
        "default": False,
        "notes": "Same checkpoint without quantisation. Larger download, more memory.",
        "supports_cloning": True,
        "gen": DOTS_GEN,
    },
    "dots-tts-soar": {
        "label": "Best quality",
        "name": "dots.tts SOAR",
        "alias": "dots-tts-soar",
        "repo": "appautomaton/dots-tts-mlx",
        "subfolder": "soar/mlx-int8",
        "licence": "Apache-2.0",
        "default": False,
        "notes": "Higher-fidelity checkpoint, roughly 3.5x slower.",
        "supports_cloning": True,
        "gen": DOTS_GEN,
    },
    "step-audio": {
        "label": "Step Audio",
        "name": "Step-Audio-EditX",
        "alias": "step-audio",
        "repo": "appautomaton/step-audio-editx-8bit-mlx",
        "subfolder": None,
        "licence": "Apache-2.0",
        "default": False,
        "notes": "Alternative engine. Cleanest install of the six evaluated; also edits audio.",
        "supports_cloning": True,
        "gen": {},
    },
    "longcat": {
        "label": "LongCat",
        "name": "LongCat-AudioDiT 3.5B",
        "alias": "longcat",
        "repo": "appautomaton/longcat-audiodit-3.5b-8bit-mlx",
        "subfolder": None,
        "licence": "MIT",
        "default": False,
        "notes": "Alternative engine. Dropped a leading word during evaluation.",
        "supports_cloning": True,
        "gen": {},
    },
}

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

# Generated clips are kept until deleted, so the workspace can list them.
CLIP_DIR = VOICE_DIR.parent / "clips"

_models: dict[str, object] = {}
_voices: dict[str, dict] = {}


def _manifest_path() -> Path:
    return VOICE_DIR / "voices.json"


def _load_voices_from_disk() -> None:
    path = _manifest_path()
    if not path.exists():
        return
    try:
        stored = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError) as exc:
        _log(f"ignoring unreadable voice manifest: {exc}")
        return
    changed = False
    for voice_id, voice in stored.items():
        # Drop entries whose audio has gone missing rather than failing later.
        audio = Path(voice.get("reference_audio", ""))
        if not audio.exists():
            _log(f"dropping voice {voice_id!r}: reference audio missing")
            continue
        # Voices saved before the length was recorded get measured once, here,
        # rather than every listing re-opening the file or the row admitting it
        # does not know something the file plainly says.
        if not voice.get("seconds"):
            try:
                info = sf.info(audio)
                voice["seconds"] = round(info.frames / info.samplerate, 1)
                changed = True
            except Exception as exc:
                _log(f"could not measure {audio}: {exc}")
        _voices[voice_id] = voice
    if changed:
        _save_voices_to_disk()
    _log(f"loaded {len(_voices)} voice(s) from {path}")


def _clips_manifest() -> Path:
    return CLIP_DIR / "clips.json"


def _load_clips() -> list[dict]:
    path = _clips_manifest()
    if not path.exists():
        return []
    try:
        clips = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError):
        return []
    for clip in clips:
        # Clips written before names existed are named from their own words,
        # which is the same rule a new clip follows.
        clip.setdefault("name", _working_name(clip.get("text", "")))
        # Clips written before takes existed are one take, described by the
        # fields that used to sit on the clip itself.
        if "takes" not in clip:
            clip["takes"] = [
                {
                    "id": f"take-{clip['id']}",
                    "path": clip.get("path", ""),
                    "audio_s": clip.get("audio_s", 0.0),
                    "gen_s": clip.get("gen_s", 0.0),
                    "seed": clip.get("seed"),
                    "created": clip.get("created", ""),
                }
            ]
        # A take whose audio has gone is not listed; a clip with none left is
        # not either, rather than showing a row that cannot play.
        clip["takes"] = [t for t in clip["takes"] if Path(t.get("path", "")).exists()]
    return [c for c in clips if c["takes"]]


def _save_clips(clips: list[dict]) -> None:
    CLIP_DIR.mkdir(parents=True, exist_ok=True)
    tmp = _clips_manifest().with_suffix(".json.tmp")
    tmp.write_text(json.dumps(clips, indent=2))
    tmp.replace(_clips_manifest())


def _save_voices_to_disk() -> None:
    VOICE_DIR.mkdir(parents=True, exist_ok=True)
    # Write-then-rename so an interrupted save cannot truncate the manifest.
    tmp = _manifest_path().with_suffix(".json.tmp")
    tmp.write_text(json.dumps(_voices, indent=2))
    tmp.replace(_manifest_path())


def _default_model() -> str:
    return next(k for k, v in MODELS.items() if v["default"])


def _load(model_id: str):
    if model_id not in MODELS:
        raise ValueError(f"unknown model {model_id!r}; have {sorted(MODELS)}")
    if model_id not in _models and not _is_installed(MODELS[model_id]):
        raise ValueError(f"model {model_id!r} is not installed")
    if model_id not in _models:
        from mlx_speech import tts

        started = time.perf_counter()
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


def _log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


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
        entry["precision"] = (
            "int8 quantised" if "int8" in sub else "full precision" if "base" in sub else "8-bit"
        )
        entry["measured_rtf"] = _measured_rtf(key)
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


def _sha256(path: Path) -> str:
    """Hash a file in chunks; reference recordings run to a few megabytes."""
    import hashlib

    digest = hashlib.sha256()
    try:
        with path.open("rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                digest.update(block)
    except OSError as exc:
        _log(f"could not hash {path}: {exc}")
        return ""
    return digest.hexdigest()


def _record_consent(voice_id: str, params: dict) -> None:
    """Append the consent that permitted this voice, and never rewrite it.

    A cloned voice is a likeness, so what matters later is not that a box was
    ticked but when, by which build, and against which recording. The log is
    append-only for the same reason: a record that can be edited answers
    nothing. Deleting the voice leaves its line — the claim was still made.
    """
    entry = {
        "voice_id": voice_id,
        "label": params.get("label", voice_id),
        "granted_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "app_version": params.get("app_version", "unknown"),
        "statement": params.get("consent_statement", ""),
        "source": params.get("source", "recording"),
        # The audio the claim was made about, hashed here rather than passed
        # in: this is the file that actually became the voice, so a later
        # dispute is about a fixed thing and not about which file was meant.
        "reference_sha256": _sha256(Path(params["reference_audio"])),
    }
    path = VOICE_DIR.parent / "consent.log"
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as log:
            log.write(json.dumps(entry) + "\n")
    except OSError as exc:
        _log(f"could not write consent record: {exc}")


def m_register_voice(params: dict) -> dict:
    """Copy the recording into app storage, then prepare it for generation.

    "Preparing" means turning the waveform into the two things the model needs
    to speak as this person: a speaker embedding (who the voice belongs to) and
    the encoded acoustic prompt (how they actually sound saying words). That
    work is seed-independent and reusable, so it is done once here rather than
    on every generation — roughly 40 seconds now against 40 seconds each time.
    """
    voice_id = params["voice_id"]
    source = Path(params["reference_audio"])
    if not source.exists():
        raise FileNotFoundError(str(source))

    # The recording lives in a temp file; copy it somewhere durable before it
    # becomes the thing a saved voice depends on.
    VOICE_DIR.mkdir(parents=True, exist_ok=True)
    stored_audio = VOICE_DIR / f"{voice_id}.wav"
    if source.resolve() != stored_audio.resolve():
        shutil.copy2(source, stored_audio)

    _record_consent(voice_id, params)
    try:
        info = sf.info(stored_audio)
        seconds = round(info.frames / info.samplerate, 1)
    except Exception as exc:
        _log(f"could not measure {stored_audio}: {exc}")
        seconds = 0.0
    _voices[voice_id] = {
        "seconds": seconds,
        "reference_audio": str(stored_audio),
        "reference_text": params.get("reference_text", ""),
        "label": params.get("label", voice_id),
        "created": time.strftime("%Y-%m-%dT%H:%M:%S"),
        # A working copy of what was agreed, so the app can show it beside the
        # voice. `consent.log` stays the record — this one travels with the
        # voice and would go with it if the voice were deleted.
        "consent": {
            "statement": params.get("consent_statement", ""),
            "app_version": params.get("app_version", "unknown"),
            "source": params.get("source", "recording"),
        },
    }
    _save_voices_to_disk()

    prepared_s = None
    if params.get("prepare", True):
        prepared_s = _prepare_voice(voice_id, params.get("model"))

    return {
        "voice_id": voice_id,
        "prepared_s": prepared_s,
        "voices": sorted(_voices),
    }


def _prepare_voice(voice_id: str, model_id: str | None = None) -> float:
    """Materialise speaker conditioning so later generations skip that cost."""
    voice = _voices[voice_id]
    model_id = model_id or _default_model()
    model = _load(model_id)
    gen = MODELS[model_id].get("gen") or {}

    started = time.perf_counter()
    prepare = getattr(model, "prepare_prompt", None)
    if prepare is not None:
        # The direct path: no waveform is synthesised, only the conditioning.
        prepare(
            voice["reference_audio"],
            reference_text=voice["reference_text"] or None,
            speaker_scale=gen.get("speaker_scale", 1.5),
        )
    else:
        # Backends without prepare_prompt warm the same cache by generating.
        kwargs = dict(gen)
        kwargs["reference_audio"] = voice["reference_audio"]
        if voice["reference_text"]:
            kwargs["reference_text"] = voice["reference_text"]
        model.generate("Ready.", **kwargs)
    return round(time.perf_counter() - started, 2)


def m_prepare_voice(params: dict) -> dict:
    voice_id = params["voice_id"]
    if voice_id not in _voices:
        raise ValueError(f"unknown voice {voice_id!r}")
    return {"voice_id": voice_id, "prepared_s": _prepare_voice(voice_id, params.get("model"))}


def m_list_voices(_params: dict) -> dict:
    return {"voices": [{"voice_id": k, **v} for k, v in _voices.items()]}


def _forget_conditioning() -> None:
    """Drop the speaker conditioning held in memory by every loaded model.

    A deleted voice leaves its recording on disk gone, but the embedding and
    acoustic prompt derived from it stay resident until the process exits —
    which is still the person's voice, in memory, after they asked for it to be
    removed. The cache is keyed on the waveform, so there is no per-voice key to
    evict from outside; clearing it wholesale is the only certain answer. The
    cost is that other voices re-prepare on next use, which is the right trade
    against keeping data someone deleted.
    """
    for model in _models.values():
        for attribute in ("_prompt_cache", "prompt_cache"):
            cache = getattr(model, attribute, None)
            if cache is not None and hasattr(cache, "clear"):
                cache.clear()
                _log(f"cleared {attribute} after deletion")


def m_delete_voice(params: dict) -> dict:
    """Remove the voice, its audio, and anything derived from it.

    Reports what it freed and how many clips were made with it. Those clips stay
    — they are the user's own output and the audio is already rendered — but the
    count is returned so the app can say so before anyone clicks.
    """
    voice_id = params["voice_id"]
    voice = _voices.pop(voice_id, None)

    freed = 0
    if voice is not None:
        audio = Path(voice.get("reference_audio", ""))
        # Confined to the app's own directory: a voice must never be able to
        # point deletion at a file somewhere else on the disk.
        if audio.exists() and audio.is_relative_to(VOICE_DIR):
            freed = audio.stat().st_size
            audio.unlink()

    _forget_conditioning()
    _save_voices_to_disk()
    clips = sum(1 for c in _load_clips() if c.get("voice_id") == voice_id)
    return {"voices": sorted(_voices), "freed_bytes": freed, "clips": clips}


# The model accepts at most 512 audio patches per call — a hard limit it
# enforces itself, not a setting. At the measured ~6.25 patches per second that
# caps one generation near 80 seconds, so longer text must be split and stitched.
MAX_AUDIO_PATCHES = 500
PATCHES_PER_SECOND = 6.25
WORDS_PER_SECOND = 3.2
# Target well under the ceiling: the estimate is rough, and running into the cap
# mid-sentence is far worse than using one extra chunk.
CHUNK_TARGET_WORDS = 110


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
            info = HfApi().model_info(spec["repo"], files_metadata=True)
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
# Generation runs as one blocking request, so stdin is not being read while it
# works. Progress and cancellation therefore travel by file: the only channel
# that stays open to a process that is busy.
_PROGRESS = VOICE_DIR.parent / "generating.json"
_CANCEL = VOICE_DIR.parent / "cancel"


class Cancelled(Exception):
    """The user stopped this generation between chunks."""


def _clear_signals() -> None:
    for path in (_PROGRESS, _CANCEL):
        try:
            path.unlink(missing_ok=True)
        except OSError:
            pass


class _Heartbeat:
    """Keep the progress file moving while a chunk is being synthesised.

    A chunk is one blocking call into the model, so the loop below can only
    report at chunk boundaries — and a short clip is a single chunk, which left
    the app showing `0.0 s elapsed` for the whole wait and then finishing. The
    thread writes the same fields between boundaries, advancing only the ones it
    actually knows: the clock. `written_s` stays where the last finished chunk
    put it, because nothing has been written since.
    """

    def __init__(self, total: int, period: float = 0.25) -> None:
        self.total = total
        self.period = period
        self.written_s = 0.0
        self.done = 0
        self._started = time.perf_counter()
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        while not self._stop.wait(self.period):
            _report(self.written_s, time.perf_counter() - self._started, self.done, self.total)

    def __enter__(self) -> "_Heartbeat":
        _report(0.0, 0.0, 0, self.total)
        self._thread.start()
        return self

    def chunk_done(self, written_s: float, done: int) -> None:
        self.written_s = written_s
        self.done = done
        _report(written_s, time.perf_counter() - self._started, done, self.total)

    def __exit__(self, *_exc: object) -> None:
        self._stop.set()
        self._thread.join(timeout=1.0)


def _report(written_s: float, elapsed_s: float, done: int, total: int) -> None:
    try:
        _PROGRESS.parent.mkdir(parents=True, exist_ok=True)
        _PROGRESS.write_text(
            json.dumps(
                {
                    "written_s": round(written_s, 2),
                    "elapsed_s": round(elapsed_s, 2),
                    "chunks_done": done,
                    "chunks": total,
                }
            )
        )
    except OSError:
        pass
# Last measured load seconds per model, from this machine.
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
            snapshot_download(spec["repo"], allow_patterns=_model_patterns(spec))
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
        "data_dir": str(VOICE_DIR.parent),
    }


def _measured_rtf(model_id: str) -> float | None:
    """Speed from this user's own clips, not a benchmark from another machine."""
    samples = [
        t["gen_s"] / t["audio_s"]
        for c in _load_clips()
        if c.get("model") == model_id
        for t in c["takes"]
        if t.get("audio_s")
    ]
    if not samples:
        return None
    return round(sum(samples) / len(samples), 2)


def m_disk_free(_params: dict) -> dict:
    usage = shutil.disk_usage(str(VOICE_DIR.parent if VOICE_DIR.exists() else Path.home()))
    return {"free_bytes": usage.free, "total_bytes": usage.total}


def m_synthesize(params: dict) -> dict:
    # `or` rather than a get() default: an absent key and an explicit JSON null
    # both mean "use the default", and a null key is what a Rust Option::None sends.
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

    voice_id = params.get("voice_id")
    if voice_id:
        voice = _voices.get(voice_id)
        if voice is None:
            raise ValueError(f"unknown voice {voice_id!r}")
        kwargs["reference_audio"] = voice["reference_audio"]
        if voice["reference_text"]:
            kwargs["reference_text"] = voice["reference_text"]

    chunks = _split_into_chunks(params["text"])
    if not chunks:
        raise ValueError("nothing to say")

    _clear_signals()
    started = time.perf_counter()
    pieces = []
    sample_rate = None
    written_s = 0.0
    with _Heartbeat(len(chunks)) as beat:
        for index, chunk in enumerate(chunks):
            # Checked between chunks rather than mid-utterance: stopping inside
            # one would leave half a sentence, and the chunk boundary is a
            # sentence end.
            if _CANCEL.exists():
                _clear_signals()
                raise Cancelled("stopped before chunk %d of %d" % (index + 1, len(chunks)))
            result = model.generate(chunk, **kwargs)
            sample_rate = result.sample_rate
            piece = np.asarray(result.waveform, dtype=np.float32).squeeze()
            pieces.append(piece)
            written_s += piece.shape[0] / sample_rate
            beat.chunk_done(written_s, index + 1)
            if index + 1 < len(chunks):
                # A short gap between chunks reads as a breath rather than a join.
                pieces.append(np.zeros(int(0.18 * sample_rate), dtype=np.float32))
    gen_s = time.perf_counter() - started
    _clear_signals()

    wav = np.concatenate(pieces) if len(pieces) > 1 else pieces[0]
    # Normalise once over the whole utterance: per-chunk normalisation would
    # make the volume step at every join.
    peak = float(np.max(np.abs(wav)))
    if peak > 0:
        wav = wav * (0.95 / peak)

    out = Path(params["output"])
    out.parent.mkdir(parents=True, exist_ok=True)
    sf.write(out, wav, sample_rate)

    audio_s = wav.shape[0] / sample_rate

    # Keep the clip unless the caller asked for a throwaway (voice warming).
    clip = None
    if params.get("keep", True):
        CLIP_DIR.mkdir(parents=True, exist_ok=True)
        now = int(time.time() * 1000)
        take = {
            "id": f"take-{now}",
            "path": "",
            "audio_s": round(audio_s, 2),
            "gen_s": round(gen_s, 2),
            "seed": int(seed),
            "created": time.strftime("%Y-%m-%dT%H:%M:%S"),
        }

        clips = _load_clips()
        # Generating again adds a take to the clip it came from rather than a
        # second clip: the words are the same, the reading is not, and the list
        # should stay one row per thing you wrote.
        existing = next(
            (c for c in clips if c["id"] == params.get("clip_id")), None
        )
        clip = existing
        if clip is None:
            text = params["text"].strip()
            clip = {
                "id": f"clip-{now}",
                # A title short enough for a sidebar row, from the words.
                "title": (text[:44] + "…") if len(text) > 45 else text,
                # A working name taken from the first words, and the user's to
                # change. `title` stays what the text says; `name` is what they
                # call it.
                "name": params.get("name") or _working_name(text),
                "text": text,
                "voice_id": voice_id,
                "model": model_id,
                "created": take["created"],
                "takes": [],
            }
            clips.insert(0, clip)

        take["path"] = str(CLIP_DIR / f"{clip['id']}-{take['id']}.wav")
        shutil.copy2(out, take["path"])
        # Newest first, which is the order the panel lists them in.
        clip["takes"].insert(0, take)
        _save_clips(clips)

    return {
        "clip": clip,
        "output": str(out),
        "model": model_id,
        "voice_id": voice_id,
        "audio_s": round(audio_s, 2),
        "gen_s": round(gen_s, 2),
        "rtf": round(gen_s / audio_s, 2) if audio_s else None,
        "seed": int(seed),
        "sample_rate": sample_rate,
        "chunks": len(chunks),
    }


def _working_name(text: str) -> str:
    """The first few words, which is what a clip is called until it is named.

    Cut on a word boundary and without trailing punctuation, because this is a
    name in a list, not a quotation.
    """
    words = text.split()
    name = " ".join(words[:5]).strip(" .,;:!?—-")
    return name or "Untitled clip"


def m_list_clips(_params: dict) -> dict:
    return {"clips": _load_clips()}


def m_duplicate_clip(params: dict) -> dict:
    """Copy a clip and its takes, audio included.

    A copy, not a reference: the point of duplicating is to have a second one
    you can change or delete without touching the first, and a shared audio
    file would make deleting either of them break the other.
    """
    clips = _load_clips()
    source = next((c for c in clips if c.get("id") == params["clip_id"]), None)
    if source is None:
        raise ValueError(f"unknown clip {params['clip_id']!r}")

    now = int(time.time() * 1000)
    copy = dict(source)
    copy["id"] = f"clip-{now}"
    copy["name"] = f"{source.get('name', '')} copy".strip()
    copy["created"] = time.strftime("%Y-%m-%dT%H:%M:%S")
    copy["takes"] = []
    CLIP_DIR.mkdir(parents=True, exist_ok=True)
    for index, take in enumerate(source.get("takes", [])):
        audio = Path(take.get("path", ""))
        if not audio.exists():
            continue
        new_take = dict(take)
        new_take["id"] = f"take-{now}-{index}"
        new_take["path"] = str(CLIP_DIR / f"{copy['id']}-{new_take['id']}.wav")
        shutil.copy2(audio, new_take["path"])
        copy["takes"].append(new_take)

    clips.insert(clips.index(source), copy)
    _save_clips(clips)
    return {"clips": clips}


def m_rename_clip(params: dict) -> dict:
    """Rename a clip. The audio and the text it was made from are untouched —
    only what it is called in the list changes."""
    clips = _load_clips()
    name = (params.get("name") or "").strip()
    for clip in clips:
        if clip.get("id") == params["clip_id"]:
            clip["name"] = name or _working_name(clip.get("text", ""))
    _save_clips(clips)
    return {"clips": clips}


def m_delete_clip(params: dict) -> dict:
    clips = _load_clips()
    keep = []
    for clip in clips:
        if clip.get("id") == params["clip_id"]:
            for take in clip.get("takes", []):
                audio = Path(take.get("path", ""))
                if audio.exists() and audio.is_relative_to(CLIP_DIR):
                    audio.unlink()
        else:
            keep.append(clip)
    _save_clips(keep)
    return {"clips": keep}


def m_ping(_params: dict) -> dict:
    return {"pong": True}


METHODS = {
    "ping": m_ping,
    "list_models": m_list_models,
    "load_model": m_load_model,
    "register_voice": m_register_voice,
    "list_voices": m_list_voices,
    "delete_voice": m_delete_voice,
    "synthesize": m_synthesize,
    "prepare_voice": m_prepare_voice,
    "list_clips": m_list_clips,
    "rename_clip": m_rename_clip,
    "duplicate_clip": m_duplicate_clip,
    "delete_clip": m_delete_clip,
    "install_model": m_install_model,
    "delete_model": m_delete_model,
    "install_status": m_install_status,
    "disk_free": m_disk_free,
    "system_info": m_system_info,
}


def main() -> None:
    _offline_by_default()
    _load_voices_from_disk()
    _log("sidecar ready")
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            print(json.dumps({"id": None, "ok": False, "error": f"bad json: {exc}"}), flush=True)
            continue

        req_id = request.get("id")
        method = METHODS.get(request.get("method", ""))
        if method is None:
            print(json.dumps({"id": req_id, "ok": False,
                              "error": f"unknown method {request.get('method')!r}"}), flush=True)
            continue

        try:
            result = method(request.get("params") or {})
            print(json.dumps({"id": req_id, "ok": True, "result": result}), flush=True)
        except Exception as exc:  # a bad request must not kill the process
            _log(traceback.format_exc())
            print(json.dumps({"id": req_id, "ok": False, "error": f"{type(exc).__name__}: {exc}"}),
                  flush=True)


if __name__ == "__main__":
    main()
