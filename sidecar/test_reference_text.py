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
_cut = _namespace["_up_to_the_last_finished_sentence"]


def as_far_as_read(script, spoken):
    """The text half of the answer, for a reading with plausible timings."""
    words = _namespace["_words"](spoken)
    heard = [(w, 0.4 * i, 0.4 * (i + 1)) for i, w in enumerate(words)]
    got = _cut(script, heard)
    return None if got is None else got[0]

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
    # Back to the last sentence they finished, not to the last word recognised:
    # a reference ending mid-clause leaves the model a clause to close, and it
    # closes it with the words it was asked to say.
    check("a truncated read falls back to a finished sentence",
          stopped.endswith("judge my calm voice."), failures)
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

    # Not one sentence finished: there is no clean pair to cut to, and a
    # reference this short was never going to clone a voice.
    check("half a sentence is no reference at all",
          as_far_as_read(SCRIPT, "My name is spoken") is None, failures)

    # The audio must be cut to the same place, or the model spends the opening
    # of what it was asked for accounting for sound the text does not cover.
    read = ("My name is spoken here and this is how I sound when I speak naturally. "
            "The quick brown fox jumps over the lazy dog, while five wizards judge my calm voice.")
    words = _namespace["_words"](read)
    heard = [(w, 0.4 * i, 0.4 * (i + 1)) for i, w in enumerate(words)]
    cut = _cut(SCRIPT, heard)
    check("the audio is kept to the end of that sentence",
          cut is not None and cut[1] >= 0.4 * len(words), failures)

    # The words that end a sentence are the ones recognition is likeliest to
    # miss — "calm voice" came back as "comfort". Rather than cut the audio at
    # the last word it did catch, which would stop mid-sentence and leave the
    # model a word of the reference to say first, it falls back to the sentence
    # before. Less reference, and still a text and an audio that describe each
    # other, which is the only thing that matters.
    misheard_end = words[:-2] + ["comfort"]
    heard = [(w, 0.4 * i, 0.4 * (i + 1)) for i, w in enumerate(misheard_end)]
    text, until = _cut(SCRIPT, heard)
    check("a misheard sentence ending falls back to the one before",
          text.endswith("naturally."), failures)
    check("and the audio is cut with it, not past it",
          until <= 0.4 * (len(_namespace["_words"](text)) + 2), failures)

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

    total = 13
    print(f"{total - len(failures)}/{total} reference-text checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
