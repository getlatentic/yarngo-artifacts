# Which release do I cut?

Two things ship, and one question decides which: **which files changed?**

|            | the application | a runtime release |
| ---------- | --- | --- |
| what it is | the `.app`/`.dmg` people install | a *dependency recipe*: `pyproject.toml` + `uv.lock` |
| carries    | Rust, UI, **`sidecar/engine.py` and `protocol.py`**, `catalog.json` (models), `packs/` (the bundled recipe floor), `tuf/root.json` | the same two recipe files, signed and versioned |
| reaches users | **manually** — they download the new `.dmg`; the app has no self-updater | automatically offered — Settings → Runtime shows "Update to X" |
| command    | `scripts/package.sh release` (with signing env) | `scripts/tuf-repo.sh publish <version>` + push |

The point of confusion, stated plainly: **the published runtime does not contain
the engine.** Published releases are `engine: bundled`, which means the Python
that actually speaks — `sidecar/engine.py`, where the audio fixes live — ships
inside the signed application and only there. The runtime channel today moves
*which packages* `uv` installs, nothing else.

## The decision table

| you changed | cut |
| --- | --- |
| anything in `crates/` (Rust, UI) | application |
| `sidecar/engine.py` or `protocol.py` | **application** (yes, really — see above) |
| `packaging/catalog.json` (models) | application |
| `packaging/packs/*/pyproject.toml` or `uv.lock` (dependencies) | **both**: runtime publish so installed apps update, and the app carries the same recipe as its offline floor |
| nothing in the app repo — a new runtime with its **own** engine | runtime only (the future third-party path; nothing published uses it yet) |

## Application release

```
APPLE_SIGNING_IDENTITY="Developer ID Application: Tosin Amuda (94SW7AUBMX)" \
APPLE_NOTARY_PROFILE=yarngo ./scripts/package.sh release
```

Then attach `target/release/yarngo studio_<version>_aarch64.dmg` to a GitHub
release. Bump `version` in the workspace `Cargo.toml` first when cutting a real
release — it names the dmg, the bundle, and the `bundled-<version>` runtime
identity.

## Runtime release

```
scripts/tuf-repo.sh publish <version>        # e.g. 2026.9.14.1
cp -R dist/tuf/. <yarngo-artifacts checkout>/tuf/
cd <that checkout> && git add tuf && git commit -m "Publish runtime <version>" && git push
```

`scripts/tuf-repo.sh status` shows the "believed until" date. Renewing it is
one command that changes nothing else:

```
scripts/tuf-repo.sh refresh
```

then copy `dist/tuf` and push as above. Everything — timestamp, snapshot,
targets — is signed 52 weeks out to match the root, so the whole repository is
one annual re-signing (`status` shows the date; the root is the wall, and a
role signed further out than root would change nothing). Missing it breaks
nothing: installed apps keep working and keep their runtime — only the update
channel goes quiet until the next publish or refresh.

## When both changed

Publish the runtime first, then cut the application. Fresh installs then get
the new recipe either way — from the repository when online, from the bundled
floor when not — and already-installed apps are offered the same recipe the
new build ships with.

## What would move engine fixes into the runtime channel

Publishing runtimes with `engine: "own"`: the archive path exists, is
TUF-verified end to end, and carries the runtime's own `engine.py`. Flipping to
it is a decision about where that code's trust comes from — today it is signed
with the application by Apple; as an own-engine runtime it would be signed by
the TUF keys instead. Worth doing the day an engine fix cannot wait for an app
release; not before.
