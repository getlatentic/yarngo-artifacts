"""What the catalogue loader must refuse.

The catalogue can arrive over the network, so this is a trust boundary: a file
fetched from anywhere decides which repositories are downloaded and which
revisions are executed. Every case below must be rejected in favour of the
bundled copy, and rejected with a line saying why — a silent fallback would
leave someone debugging stale models with no clue the fetch was ignored.

Not part of `cargo test`: it needs an interpreter with the sidecar's own
dependencies. Run it against either pack's environment:

    <pack>/.venv/bin/python3 sidecar/test_catalog.py
"""

import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import engine  # noqa: E402

BUNDLED = Path(__file__).resolve().parent.parent / "packaging" / "catalog.json"

# Each case mutates a copy of the real catalogue in exactly one way.
BROKEN = {
    "a schema this sidecar predates": lambda c: c.update(schema=2),
    "an api this sidecar cannot honour": lambda c: c.update(sidecar_api=99),
    "no entries for the backend": lambda c: c["backends"].pop("mlx"),
    "an empty backend": lambda c: c["backends"].update(mlx={}),
    "an entry with no revision": lambda c: c["backends"]["mlx"]["dots-tts-mf"].pop("revision"),
    "an entry with no licence": lambda c: c["backends"]["mlx"]["dots-tts-mf"].update(licence=""),
    "an entry with no repository": lambda c: c["backends"]["mlx"]["dots-tts-mf"].update(repo=""),
    "nothing marked default": lambda c: [
        e.update(default=False) for e in c["backends"]["mlx"].values()
    ],
}


def main() -> int:
    good = json.loads(BUNDLED.read_text())
    scratch = Path(tempfile.mkdtemp())
    failures = []

    for description, mutate in BROKEN.items():
        payload = json.loads(json.dumps(good))
        mutate(payload)
        path = scratch / "broken.json"
        path.write_text(json.dumps(payload))
        if engine._read_catalog(path, "mlx") is not None:
            failures.append(f"accepted {description}")

    truncated = scratch / "truncated.json"
    truncated.write_text(json.dumps(good)[:200])
    if engine._read_catalog(truncated, "mlx") is not None:
        failures.append("accepted a truncated file")

    if engine._read_catalog(scratch / "absent.json", "mlx") is not None:
        failures.append("accepted a file that does not exist")

    # And the bundled catalogue itself must pass, for every backend it claims,
    # or the app ships unable to load its own models.
    for backend in good["backends"]:
        if engine._read_catalog(BUNDLED, backend) is None:
            failures.append(f"refused the bundled catalogue for {backend}")

    for failure in failures:
        print(f"FAIL: {failure}", file=sys.stderr)
    checks = len(BROKEN) + 2 + len(good["backends"])
    print(f"{checks - len(failures)}/{checks} catalogue checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
