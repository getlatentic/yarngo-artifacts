# yarngo artifacts

The model catalogue and runtime recipes for **yarngo studio**, published so they
can be corrected without shipping a new application.

Everything here is a *recipe*, never a payload. The catalogue names Hugging Face
repositories at pinned, immutable revisions; the runtime packs are a
`pyproject.toml` and a `uv.lock`. Every heavy byte still comes from wherever it
already lived — model weights from Hugging Face, CPython from
python-build-standalone, wheels from PyPI. Nothing here is more than a few
hundred kilobytes.

## How the app reads this

The application knows exactly one URL:

```
https://github.com/getlatentic/yarngo-artifacts/releases/download/latest/manifest.json
```

`latest` is a moving pointer holding nothing but that manifest. Everything the
manifest names is pinned to a dated, immutable `catalog-*` tag and carries a
SHA-256 that the app verifies before use. So the URL compiled into a shipped
binary never changes, while nothing it actually consumes is mutable.

A publication is refused by the app, and the copy it shipped with is used
instead, if the schema is unrecognised, the required sidecar API is newer than
the app implements, the app is older than `min_app_version`, a digest does not
match, or any catalogue entry is missing a repository, revision or licence.
**The bundled copy is the floor**: yarngo studio works with no network at all,
and a bad publication here can make it stale but cannot break it.

## Releases

| Tag | Holds |
| --- | --- |
| `latest` | `manifest.json` only — the moving pointer |
| `catalog-<timestamp>` | the catalogue and the runtime recipes it names |

Published by `scripts/publish-artifacts.sh` in the application repository.

## What is not here

Application builds, model weights, and interpreters. The application is
distributed separately; weights and interpreters come from their own upstreams,
which is why this repository stays small enough to be read at a glance.
