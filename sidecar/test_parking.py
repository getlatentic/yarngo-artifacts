"""A model that has stopped speaking gives its gigabytes back.

Loaded weights are held against the possibility of another clip. The
possibility does not justify the residence indefinitely: after long enough
silent the model is dropped and reloaded on demand, trading seconds of load —
already shown in the interface — for gigabytes of memory. Listing models and
drawing the interface never count as speaking, so watching the app never keeps
it heavy.
"""

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
_source = (HERE / "engine.py").read_text()
_namespace = {"os": __import__("os")}
exec(  # noqa: S102 - our own source, without importing the model runtime
    _source[
        _source.index("def _park_due") : _source.index("def _park_idle_models")
    ],
    _namespace,
)
due = _namespace["_park_due"]


def check(name, condition, failures):
    print(f"  {'ok  ' if condition else 'FAIL'}  {name}")
    if not condition:
        failures.append(name)


def main() -> int:
    failures: list = []

    check("a model silent past the bar is due",
          due({"mf": 100.0}, 800.0, 600.0) == ["mf"], failures)
    check("a model that just spoke is kept",
          due({"mf": 750.0}, 800.0, 600.0) == [], failures)
    check("only the silent one of two goes",
          due({"mf": 100.0, "soar": 790.0}, 800.0, 600.0) == ["mf"], failures)
    check("exactly at the bar counts as silent",
          due({"mf": 200.0}, 800.0, 600.0) == ["mf"], failures)
    check("parking switched off parks nothing, ever",
          due({"mf": 0.0}, 1e9, 0.0) == [], failures)

    total = 5
    print(f"{total - len(failures)}/{total} parking checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
