"""The reference text must describe the reference audio.

A model told about words the recording does not contain speaks them before it
speaks anything it was asked for — they are in its prompt and it finishes them
first. Observed in the field: every clip began "and the way I shape my words",
the tail of an enrolment script the reader had stopped just short of.

These check the part that decides where to cut. Recognition itself is not
exercised here; it needs a model and a machine that can run one.
"""

import difflib
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

# Loaded without importing the engine, which pulls in the whole speech stack.
_source = (HERE / "engine.py").read_text()
_namespace = {"re": re, "difflib": difflib}
exec(  # noqa: S102 - reading our own source, to avoid importing a model runtime
    _source[_source.index("def _words(") : _source.index("def m_audio_prepare_reference")],
    _namespace,
)
as_far_as_read = _namespace["_script_as_far_as_read"]

SCRIPT = (
    "My name is spoken here, and this is how I sound when I speak naturally. "
    "The quick brown fox jumps over the lazy dog, while five wizards judge my calm voice. "
    "I am recording this so the app can learn my accent, my rhythm, and the way I shape my words."
)


def check(name, condition, failures):
    print(f"  {'ok  ' if condition else 'FAIL'}  {name}")
    if not condition:
        failures.append(name)


def main() -> int:
    failures: list = []

    whole = as_far_as_read(SCRIPT, SCRIPT)
    check("a complete read keeps the whole script", whole == SCRIPT, failures)

    # The recording this was written for: the reader stopped before the last
    # phrase, and recognition also misheard several words along the way.
    stopped = as_far_as_read(
        SCRIPT,
        "My name is Popone here and this is i sound when i speak naturally. The quick brown "
        "fox jumps over an easy dog, while five wizards judge my calm voice. I'm recording "
        "this so the app can learn my accent, my rhythm",
    )
    check("a truncated read is cut where it stopped", stopped.endswith("my rhythm"), failures)
    check("and does not keep what was never said", "shape my words" not in stopped, failures)

    # Mishearing must not be read as stopping. A walk that needs each next word
    # exactly stops at the first mistake, which reports a fluent reader as
    # having said almost nothing.
    misheard = as_far_as_read(
        SCRIPT,
        "My name is broken hair, and this is how eye sound when I speek naturally. The quick "
        "brown fox jumped over the lazy dog, while five wizzards judge my calm voice. I am "
        "recording this so the app can learn my accent, my rhythm, and the way I shape my words.",
    )
    check("words misheard throughout still count as read", misheard == SCRIPT, failures)

    one = as_far_as_read(SCRIPT, "My name is spoken here and this is how I sound when I speak naturally.")
    check("one sentence read is one sentence kept", one.endswith("naturally."), failures)
    check("the script's own punctuation survives", one.count(",") == 1, failures)

    # Cutting short costs conditioning quality; cutting long is the defect
    # itself. Everything unclear resolves towards short.
    for label, spoken in (
        ("nothing recognisable", "bonjour monsieur comment allez vous"),
        ("silence", ""),
    ):
        check(
            f"{label} says nothing rather than guessing",
            as_far_as_read(SCRIPT, spoken) is None,
            failures,
        )

    check("an empty script cannot be read from", as_far_as_read("", "anything") is None, failures)

    total = 9
    print(f"{total - len(failures)}/{total} reference-text checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
