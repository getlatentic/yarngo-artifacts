# yarngo artifacts

The runtime recipes for **yarngo studio**, published so a runtime can be
corrected or replaced without shipping a new application.

Everything here is a *recipe*, never a payload: a `pyproject.toml` and a
`uv.lock`. Every heavy byte still comes from wherever it already lived — model
weights from Hugging Face, CPython from python-build-standalone, wheels from
PyPI. Nothing here is more than a few hundred kilobytes.

## How the app reads this

Under `tuf/` is a [TUF](https://theupdateframework.io) repository. The
application ships the root role — the public half of the keys that sign this —
inside its own signed bundle, and checks everything it fetches against it.

```
https://raw.githubusercontent.com/getlatentic/yarngo-artifacts/main/tuf/
```

That settles the two questions a checksum in a manifest cannot. **Who
published this**: metadata signed by keys the application was built trusting,
so anyone who can write to this repository still cannot make it install
anything. And **is this current**: the application remembers the metadata
version it last saw, so serving it an older repository — every signature
genuine, nothing expired — is refused as a replay rather than accepted as an
update.

Signing roles are separate. The root role needs two of three keys, so one lost
is recoverable and one stolen is not enough. The timestamp key is re-signed
most often and is therefore the most exposed; it can say a snapshot is current
and cannot say which targets are ours.

`tuf/targets/catalogue.json` lists each runtime's releases. A release names its
files, the oldest application that can use it, and the engine API it speaks —
so an application passes over a release it could not drive, rather than
installing something it cannot talk to.

**The recipe inside the application is the floor.** yarngo studio installs and
runs with no network at all; a bad or unreachable publication here can leave it
on an older runtime but cannot break it.

## Publishing

From the application repository:

```
scripts/tuf-repo.sh publish <version>
```

then copy `dist/tuf/` here and push. Timestamp metadata expires seven days
after it is signed — that is what freshness means, so republishing is also how
this stays believed.

## Superseded

The `latest` and `catalog-*` release tags are from an earlier scheme that named
files by URL and digest in an unsigned manifest. Nothing reads them.

## Application builds

Notarized `.dmg` builds of yarngo studio are attached to the
[Releases](https://github.com/getlatentic/yarngo-artifacts/releases) of this
repository — release assets, not repository contents, so the tree stays small.

## What is not here

Model weights and interpreters, which come from their own upstreams, and the
application's source. Runtime recipes and application builds are the two
things this repository distributes.
