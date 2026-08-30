"""The shape of the audio the engine hands back.

Two defects this guards, both found by listening to real output. A clip that
starts speaking at sample zero loses its first word to anything that ramps up —
a DAC, Bluetooth, a transcriber deciding where speech begins; the word was in
the file the whole time. And a model can leave many seconds of dead air inside
a clip — seven were measured in a forty-word one — which is a defect, not a
breath.
"""

import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
_source = (HERE / "engine.py").read_text()
_namespace = {"np": np}
exec(  # noqa: S102 - our own source, without importing the model runtime
    _source[
        _source.index("LEAD_IN_S = ") : _source.index(
            "def m_synthesis_generate"
        )
    ],
    _namespace,
)
settle = _namespace["_settle_edges_and_pauses"]
LEAD_IN_S = _namespace["LEAD_IN_S"]

SR = 24000


def tone(seconds):
    t = np.arange(int(seconds * SR)) / SR
    return (0.3 * np.sin(2 * np.pi * 150 * t)).astype(np.float32)


def hush(seconds):
    # Model silence is never exact zeros — it is a low noise floor, and a
    # threshold that only sees true zero would pass every test and fail every
    # real clip.
    rng = np.random.default_rng(7)
    return rng.normal(0.0, 2e-4, int(seconds * SR)).astype(np.float32)


def check(name, condition, failures):
    print(f"  {'ok  ' if condition else 'FAIL'}  {name}")
    if not condition:
        failures.append(name)


def main() -> int:
    failures: list = []

    # Every clip gets a lead-in, so the first word survives whatever plays it.
    voiced = tone(1.0)
    out = settle(voiced, SR)
    head = out[: int(0.15 * SR)]
    check("a clip no longer starts speaking at sample zero",
          len(out) >= len(voiced) + int(0.15 * SR)
          and float(np.abs(head).max(initial=0.0)) < 1e-6, failures)

    # A breath is delivery and is left exactly alone.
    speech = np.concatenate([tone(2.0), hush(1.5), tone(2.0)])
    out = settle(speech, SR)
    check("a pause anyone might leave is not touched",
          abs(len(out) - len(speech) - int(LEAD_IN_S * SR)) < SR // 100, failures)

    # Dead air is collapsed to a breath, not removed.
    speech = np.concatenate([tone(2.0), hush(7.0), tone(2.0)])
    out = settle(speech, SR)
    expected = int((2.0 + 0.8 + 2.0 + LEAD_IN_S) * SR)
    check("seven seconds of dead air becomes a breath",
          abs(len(out) - expected) < SR // 4, failures)
    # and the speech on both sides is intact
    check("the speech around it is untouched",
          float(np.abs(out[-SR:]).max()) > 0.2, failures)

    # A dragging tail is trimmed to a close, not an outro.
    speech = np.concatenate([tone(2.0), hush(3.0)])
    out = settle(speech, SR)
    expected = int((2.0 + 0.4 + LEAD_IN_S) * SR)
    check("a three-second tail becomes a close",
          abs(len(out) - expected) < SR // 4, failures)

    # Quiet is judged against this clip's own level, not an absolute number.
    softly = np.concatenate([tone(2.0) * 0.05, hush(7.0), tone(2.0) * 0.05])
    out = settle(softly, SR)
    check("a quiet speaker's dead air is still dead air",
          len(out) < len(softly) - 4 * SR, failures)

    # Silence the model puts before its first word is not a pause — measured
    # at nearly four seconds on one clip, which is four seconds of a listener
    # wondering whether anything played. The lead-in is the onset.
    speech = np.concatenate([hush(3.8), tone(2.0)])
    out = settle(speech, SR)
    expected = int((LEAD_IN_S + 2.0) * SR)
    check("dead air before the first word does not survive",
          abs(len(out) - expected) < SR // 4, failures)

    total = 7
    print(f"{total - len(failures)}/{total} output-audio checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
