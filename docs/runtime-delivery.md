# Delivering a runtime

A runtime is downloaded, and it contains code the application executes. That
makes this a software update system, and the parts of one that are about trust
are not written here.

## What is ours and what is not

| | |
| --- | --- |
| Rust ↔ runtime | JSON-RPC 2.0 over stdio, `yarngo-engine` version 1 |
| Who published this and is it current | The Update Framework |
| What a runtime is and how it starts | the descriptor, in `runtimes/<id>/runtime.json` |
| What it can do | the handshake's method list |

The middle row is the one that must not be invented. Freshness, rollback,
expiry, key rotation and signing thresholds are a solved problem with a
specification, an audit history and Rust clients; growing our own would mean
reinventing all of it and getting some of it wrong quietly.

## Where this stands

The digest-in-a-manifest machinery is gone. A release names TUF targets, and
what those targets are and whether they are current is settled by metadata
signed with keys the application ships trusting. `scripts/tuf-repo.sh` creates
the repository and the root role; until a root role is shipped, nothing is
published to this application and an install uses the recipe that shipped —
which is what a machine with no network does anyway.

## What is already right, and stays

These are ours under any trust model, and are done:

- **Extraction.** Every object an archive materialises belongs underneath the
  directory it is installed into. Absolute paths, `..`, symlinks, hard links,
  devices and fifos are refused rather than skipped or repaired. Entry count,
  entry size and total size are capped.
- **The descriptor is not a way to run things.** It chooses a program inside
  the runtime's own directory or inside the environment installed for it, never
  absolute and never climbing out. The environment is the application's to set,
  because a descriptor that could set `PYTHONPATH` would be running its own
  code without naming a program.
- **Fallback is explicit.** A runtime says whether its engine is its own or the
  bundled one. One that said its own and has none does not start; speaking this
  protocol is not being the same implementation.
- **The handshake is the authority on capability.** A manifest saying a runtime
  speaks version 1 is a claim; what the process answers at `initialize` is the
  fact, and the application offers only what that list permits.

## The client

`tough` is the Rust TUF client, wrapped in `speech-engine::trust`. Its own HTTP
client is left out: fetching stays on the `curl` the installer already uses, so
proxies, retries and certificate handling keep behaving the way they do on the
machines this runs on. Only verification is TUF's.

Downloads go over `https` or a local `file` URL and nothing else — TUF would
still catch tampering over plain HTTP, but there is no reason to let anyone
watch. A target is read back in pieces and written as it arrives, because a
runtime archive is hundreds of megabytes and never belongs in memory whole.

`crates/speech-engine/tests/runtime_trust.rs` shows the client answering the two
questions a digest cannot. Against repositories built by
`scripts/make-tuf-fixture.sh` — one ours, one another publisher's, one signed
correctly and long expired — loading refuses:

- a target edited after signing, and metadata edited after signing;
- **a correctly signed repository whose keys were never ours**, serving the very
  same bytes we would have served;
- metadata that has expired;
- **last week's repository, replayed by the publisher who signed it** — every
  signature genuine, nothing expired, and caught only because what was already
  seen is remembered. Serving old metadata is how someone holds a machine on the
  version they already know how to break, and it needs no keys at all.

Each of those is asserted on the reason it was refused, not merely that it was,
so a fixture that quietly went missing cannot satisfy them; and each tamper test
loads the untouched copy first, so the refusal is the edit. Roles hold separate
keys, so losing the timestamp key does not mean losing the ability to say which
targets are ours.

## What comes next

1. Put the runtime archive behind TUF: the archive becomes a target, and its
   metadata is what says the target is current and authentic. Yarngo's own
   fields — runtime id, version, platform, engine API — travel beside the
   target rather than carrying the security. Then the custom digest-in-a-manifest
   machinery goes, rather than sitting alongside as a second answer.
2. Install to a staging directory, complete the handshake from staging, and
   promote atomically. A failure anywhere leaves the runtime that was working
   in place, and keeping the previous version is then rollback for free.
3. Record which runtime and which version made a clip, now that a release has
   an identity to record.

Only then is the archive path worth opening.

## The catalogue

What runtimes exist and at which versions is itself a target, so what it says is
covered by the same signatures as the archives it names. It carries no URLs and
no digests: where a target lives and what it hashes to is already the
repository's to say, and a second answer to a settled question is only a way to
disagree.

Its three version fields answer three different questions, and are deliberately
not one field:

| `schema` | can this build read this document |
| --- | --- |
| `engine_api` | can this build hold a conversation with that engine |
| `version` | which implementation is installed |

A release naming a later `engine_api` is passed over rather than half-understood
— that refusal is what makes publishing a runtime without shipping an
application safe. When nothing fits, the reason each release was passed over is
what tells someone whether to update the application or wait.

## The boundary of "a new runtime without a new application"

A runtime can be published without releasing the application when it fits
metadata this build can read, speaks an engine API this build implements, and
needs only capabilities this build already has. A runtime wanting streaming
output, several models at once, or a licence flow that Engine API 1 has no way
to express still needs the application to move. That is the normal shape of a
plugin contract, and it is worth saying out loud so the promise is not larger
than it is.
