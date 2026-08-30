"""Chunks are sized for the voice, not for an average nobody recorded.

The model has one patch budget for reference and output together. A longer
reference leaves less room to speak, and a fixed word target then runs into the
ceiling and stops mid-word — measured: a 42-second reference left 37 of 80
seconds, and 99 words came back as 92.
"""

import difflib  # noqa: F401 - the sliced source expects the engine's imports
import math
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
_source = (HERE / "engine.py").read_text()
_namespace = {"math": math, "re": re}
exec(  # noqa: S102 - our own source, without importing the model runtime
    _source[_source.index("CHUNK_TARGET_WORDS = ") : _source.index("def _patch_seconds")],
    _namespace,
)
target = _namespace["_calibrated_target_words"]
CEILING = _namespace["CHUNK_TARGET_WORDS"]
FLOOR = _namespace["CHUNK_FLOOR_WORDS"]

PATCH = 0.16  # dots: 160 ms of audio per patch


def check(name, condition, failures):
    print(f"  {'ok  ' if condition else 'FAIL'}  {name}")
    if not condition:
        failures.append(name)


def main() -> int:
    failures: list = []

    # The measured failure: 42s reference at this voice's pace (14.1s / 31
    # words). 99 words were sent as one chunk and cut at the ceiling.
    words = target(PATCH, 500, 42.3, 93)
    check("the 42-second-reference cliff gets chunks that fit",
          words * (42.3 / 93) <= (500 - 263 - 2) * PATCH, failures)
    check("and they are real chunks, not confetti", words >= FLOOR, failures)

    # An ordinary reference changes little: the ceiling still applies.
    check("a 14-second reference keeps chunks near the ceiling",
          target(PATCH, 500, 14.1, 31) >= 90, failures)

    # A monster reference: chunks shrink hard, still fit, never below the floor.
    monster = target(PATCH, 500, 55.0, 120)
    monster_budget = (500 - (math.ceil(55.0 / PATCH) - 1) - 2) * PATCH
    check("a 55-second reference shrinks chunks to what still fits",
          FLOOR <= monster < 50 and monster * (55.0 / 120) <= monster_budget, failures)

    # No reference: the model's own voice has almost the whole budget.
    check("no reference means chunks near the ceiling",
          target(PATCH, 500, 0.0, 0) >= 90, failures)

    # A backend without patch geometry keeps today's behaviour.
    check("no patch geometry means no change",
          target(None, 0, 30.0, 60) == CEILING, failures)

    # Reference audio without text: pace unknown, assume slow rather than cut.
    slow_guess = target(PATCH, 500, 30.0, 0)
    with_pace = target(PATCH, 500, 30.0, 66)  # same audio, 0.45s/word known
    check("an unknown pace is assumed slower than a known one",
          slow_guess <= with_pace, failures)

    total = 7
    print(f"{total - len(failures)}/{total} chunk-budget checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
