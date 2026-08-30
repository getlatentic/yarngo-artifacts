"""A generation that ran out of budget is never kept.

Sizing chunks ahead of time rests on a guess about pace. Guesses are allowed
to be wrong; correctness is not. Whether a piece hit the model's output
ceiling is arithmetic on known quantities — budget, patch size, reference
length — so the guess is checked after the fact and a truncated piece is
regenerated in halves rather than shipped with its sentence amputated.
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
    _source[_source.index("def _speakable_seconds") : _source.index("def _patch_seconds")],
    _namespace,
)
speakable = _namespace["_speakable_seconds"]
hit = _namespace["_hit_ceiling"]
split = _namespace["_split_for_retry"]

PATCH = 0.16


def check(name, condition, failures):
    print(f"  {'ok  ' if condition else 'FAIL'}  {name}")
    if not condition:
        failures.append(name)


def main() -> int:
    failures: list = []

    # The measured case: 42.3s reference against a 500-patch budget.
    ceiling = speakable(PATCH, 500, 42.3)
    check("the ceiling is arithmetic, not a guess",
          abs(ceiling - (500 - (math.ceil(42.3 / PATCH) - 1) - 2) * PATCH) < 1e-9, failures)

    check("a piece at the ceiling is caught", hit(ceiling - 0.01, ceiling, PATCH), failures)
    check("a piece one patch under is caught",
          hit(ceiling - PATCH + 0.01, ceiling, PATCH), failures)
    check("a finished piece well under is kept", not hit(ceiling - 5.0, ceiling, PATCH), failures)
    check("no geometry means no verdict", not hit(100.0, None, None), failures)

    # Splitting: sentences stay whole while there is more than one.
    two = split("One thing was said. Another thing was said after it.")
    check("a two-sentence chunk splits at the sentence",
          two == ["One thing was said.", "Another thing was said after it."], failures)

    lopsided = split("A. B. C. D. E.")
    check("five sentences split near the middle",
          lopsided is not None and len(lopsided[0].split()) == 2, failures)

    # One long sentence: the comma nearest the middle, so the seam lands on a
    # breath that was already written.
    one = split("The meeting starts at four, and it runs for an hour, and nobody may leave early.")
    check("a single sentence splits at the middle comma",
          one is not None and one[0].endswith("hour,"), failures)

    plain = split("word " * 20)
    check("no commas still splits between words",
          plain is not None and abs(len(plain[0].split()) - 10) <= 1, failures)

    check("a fragment too small to split says so", split("too small to cut") is None, failures)

    total = 10
    print(f"{total - len(failures)}/{total} truncation-guard checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
