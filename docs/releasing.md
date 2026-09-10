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
scripts/tuf-repo.sh serve                    # onto gh-pages, which Pages serves
```

The served files live on the `gh-pages` branch, never on `main`: a publish
rewrites every metadata file and adds targets that are never deleted, and none
of that belongs in a source diff.

There is no recurring signing duty. Everything — root included — is signed a
century out, by decision: what expiry would buy is freeze-detection and a
passive kill for leaked keys, and what remains without it is everything that
stops anyone *changing* what is served — signatures, per-file hashes, role
separation, and the monotonic metadata version that refuses rollbacks. (For
scale: Sparkle, the de facto standard for Mac app updates, is a signed feed
with no expiry at all.) The trade, written down: if a signing key ever leaks,
the recovery is shipping an application with a new root, and installs that
never update stay exposed to that key. `scripts/tuf-repo.sh refresh` re-signs
what is already served and exists for key rotation, not for a calendar.
Re-introduce real expiry windows when signing stops being one laptop.

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
