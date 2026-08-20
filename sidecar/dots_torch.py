"""The torch backend: upstream dots.tts checkpoints behind the same catalogue.

The MLX backend runs int8 conversions on Apple silicon. This one runs the
checkpoints those conversions were made *from* — pinned to the same immutable
revisions — under PyTorch, which is what Windows and Linux get. Same ids, same
labels, so a clip made on either platform names a model the other recognises;
what differs is the artifact and the runtime, which is exactly the separation
the catalogue records.

Importable without torch installed: the heavy imports happen at load, so the
engine can ask whether this backend exists without paying for it.

Upstream installs here without WeTextProcessing (its pynini dependency has no
Windows wheels, for text normalisation the runtime defaults to off). Its text
module still imports `tn.*` at module scope, so a stub is registered first:
imports succeed, and anything that actually *uses* normalisation fails loudly
instead of normalising wrongly.
"""

import sys
import types
from types import SimpleNamespace

# Same ids as the MLX catalogue, deliberately. "dots-tts-mf" means dots.tts MF
# everywhere; which artifact serves it is this file's business.
MODELS = {
    "dots-tts-mf": {
        "label": "Fast",
        "name": "dots.tts MF",
        "alias": "dots-tts-mf",
        "repo": "dots-studio/dots.tts-mf",
        # The revision the MLX int8 artifact was converted from, so the two
        # backends are the same logical model by construction, not by claim.
        "revision": "25c53fb462e57087e52237daa5ea30df1c5cc328",
        "subfolder": None,
        "licence": "Apache-2.0",
        "precision": "bf16",
        "default": True,
        "notes": "Validated default. Upstream checkpoint under PyTorch.",
        "supports_cloning": True,
        # The settings DOTS_GEN validated across 8 Nigerian speakers, in
        # upstream's vocabulary. Left at upstream defaults, this checkpoint
        # dropped the leading clause of a sentence in live runs — the same
        # behaviour the evaluation tuned away. max_audio_patches (500) and
        # eos_threshold (0.8) are already upstream's constructor defaults.
        "gen": {"guidance_scale": 1.2, "speaker_scale": 1.5, "template_name": "tts"},
    },
    "dots-tts-soar": {
        "label": "Best quality",
        "name": "dots.tts SOAR",
        "alias": "dots-tts-soar",
        "repo": "dots-studio/dots.tts-soar",
        "revision": "e3520f75254d0020a0406db31c51a79d00d22d55",
        "subfolder": None,
        "licence": "Apache-2.0",
        "precision": "bf16",
        "default": False,
        "notes": "Higher-fidelity checkpoint, markedly slower off-GPU.",
        "supports_cloning": True,
        "gen": {"guidance_scale": 1.2, "speaker_scale": 1.5, "template_name": "tts"},
    },
}

# The keyword arguments generate() forwards to upstream. An explicit list
# rather than **: a setting that silently went nowhere is how "it ignored my
# steps" bugs are born, so anything else raises.
_GENERATE_OPTIONS = {
    "template_name",
    "language",
    "speaker_scale",
    "ode_method",
    "num_steps",
    "guidance_scale",
}

# Handled here, never forwarded.
_BACKEND_OPTIONS = {"transcript_prefill"}


def _stub_tn() -> None:
    """Satisfy `from tn.… import Normalizer` without WeTextProcessing.

    dots_tts.utils.text imports the normalisers at module scope; the runtime
    never calls them while `normalize_text` stays False, which this backend
    pins. Using one anyway must fail as a statement, not as a mystery.
    """
    if "tn" in sys.modules:
        return

    class _Refuse:
        def __init__(self, *args, **kwargs):
            raise RuntimeError(
                "text normalisation is not bundled in this runtime; "
                "it is off by default and nothing here turns it on"
            )

    for name in (
        "tn",
        "tn.chinese",
        "tn.english",
        "tn.chinese.normalizer",
        "tn.english.normalizer",
    ):
        sys.modules.setdefault(name, types.ModuleType(name))
    sys.modules["tn.chinese.normalizer"].Normalizer = _Refuse
    sys.modules["tn.english.normalizer"].Normalizer = _Refuse


class _Model:
    """One loaded checkpoint, presenting the surface the engine generates
    against: `generate(text, …) -> (waveform, sample_rate)`."""

    def __init__(self, spec: dict):
        _stub_tn()
        import torch
        from dots_tts.runtime import DotsTtsRuntime

        self._torch = torch
        # Upstream picks cuda-else-cpu internally and warns that a silent CPU
        # fall-back under bf16 causes dtype mismatches — so choose the
        # precision by the device it will actually run on.
        precision = "bfloat16" if torch.cuda.is_available() else "float32"
        self._runtime = DotsTtsRuntime.from_pretrained(
            spec["repo"],
            revision=spec["revision"],
            precision=precision,
        )

    def generate(
        self,
        text: str,
        *,
        seed: int,
        reference_audio: str | None = None,
        reference_text: str | None = None,
        **options,
    ):
        unknown = set(options) - _GENERATE_OPTIONS - _BACKEND_OPTIONS
        if unknown:
            raise TypeError(f"options this backend does not take: {sorted(unknown)}")

        # Transcript-conditioned prompt prefill is OFF here, deliberately, and
        # the reference transcript is not sent unless it is switched back on.
        #
        # Measured on 20 Aug 2026 (macOS CPU, MF, two seeds): with the
        # transcript supplied, upstream deterministically dropped the leading
        # clause of the target sentence; without it, every word arrived. The
        # candidate mechanism is one span of prompt accounting — upstream ceils
        # the reference into whole patches and prefills all of them, while the
        # validated MLX port deliberately keeps the final partial patch out
        # (`ceil - 1` in its `_estimate_prompt_patch_count`). Speaker-only
        # conditioning costs a little likeness (0.965 vs 0.985 against the
        # same reference) and buys back the words, and wrong words are the
        # worse failure. Revisit on CUDA hardware where a run takes seconds,
        # and with upstream, where the fix belongs.
        if not options.pop("transcript_prefill", False):
            reference_text = None

        # Upstream has no seed parameter; determinism is the caller's to set
        # up. Global rather than a local generator because upstream calls
        # torch's default RNG internally.
        self._torch.manual_seed(int(seed))
        if self._torch.cuda.is_available():
            self._torch.cuda.manual_seed_all(int(seed))

        result = self._runtime.generate(
            text=text,
            prompt_audio_path=reference_audio,
            prompt_text=reference_text,
            normalize_text=False,
            **options,
        )
        audio = result["audio"]
        waveform = (
            audio.detach().to(self._torch.float32).cpu().numpy().squeeze()
            if self._torch.is_tensor(audio)
            else audio
        )
        return SimpleNamespace(
            waveform=waveform,
            sample_rate=int(result.get("sample_rate") or self._runtime.sample_rate),
        )


def load(spec: dict) -> _Model:
    return _Model(spec)
