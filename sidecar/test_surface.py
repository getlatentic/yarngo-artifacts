"""What the engine answers, checked against what is written down about it.

Two things drift quietly. A method added here and not in the document leaves the
document describing an engine that no longer exists; a method removed and left
in the document sends somebody looking for it. Both are cheap to catch and
neither is caught by anything else — the conformance suite tests the serving
layer, and knows nothing about which operations are mounted on it.

Read rather than imported: importing the engine needs the model stack, and this
needs to run anywhere.

    python3 sidecar/test_surface.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ENGINE = HERE / "engine.py"
PROTOCOL = HERE / "protocol.py"
DOC = HERE.parent / "docs" / "engine-surface.md"

# Answered by the serving layer itself rather than mounted by the engine, so
# they appear in neither dictionary and still have to be documented.
SERVED_BY_THE_PROTOCOL = {"initialize", "job.cancel"}


def mounted(name: str) -> set[str]:
    """The method names in one of the engine's two dispatch tables."""
    block = re.search(rf"^{name}[^{{]*{{(.*?)^}}", ENGINE.read_text(), re.S | re.M)
    assert block, f"no {name} in engine.py"
    return set(re.findall(r'"([^"]+)":', block.group(1)))


def documented() -> set[str]:
    return set(re.findall(r"\| `([a-z_.]+)` \|", DOC.read_text()))


def check(name: str, condition: bool, failures: list) -> None:
    if not condition:
        failures.append(name)


def main() -> int:
    failures: list[str] = []
    served = mounted("JSONRPC_BROKER") | mounted("JSONRPC_MODEL") | SERVED_BY_THE_PROTOCOL
    written = documented()

    missing = sorted(served - written)
    check(f"every method is written down (missing: {missing})", not missing, failures)
    invented = sorted(written - served)
    check(f"nothing is written down that is not served (extra: {invented})", not invented, failures)

    # The one rule the surface exists to keep. Storage words are not a perfect
    # test of intent, but a method named for one is worth stopping to look at.
    storage = sorted(m for m in served if re.search(r"clip|voice(?!_)|consent", m))
    check(
        f"nothing here answers for what the application keeps (found: {storage})",
        not storage,
        failures,
    )

    # The version the document names is the version the code sends.
    stated = re.search(r"`yarngo-engine` version (\d+)", DOC.read_text())
    actual = re.search(r"^PROTOCOL_VERSION\s*=\s*(\d+)", PROTOCOL.read_text(), re.M)
    check(
        "the document names the version the engine sends",
        stated and actual and stated.group(1) == actual.group(1),
        failures,
    )

    for failure in failures:
        print(f"FAIL: {failure}", file=sys.stderr)
    total = 4
    print(f"{total - len(failures)}/{total} surface checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
