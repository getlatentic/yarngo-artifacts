# Delivering a runtime

A runtime is downloaded, and it contains code the application executes. That
makes this a software update system, and the parts of one that are about trust
are not written here.

## What is ours and what is not

| | |
| --- | --- |
| Rust ↔ runtime | JSON-RPC 2.0 over stdio, `yarngo-engine` version 1 |
| Who published this and is it current | The Update Framework |
| What a runtime is and how it starts | the descriptor, beside it in its version directory |
| Which version answers | one row in the application's database |
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

## Publishing

`scripts/tuf-repo.sh init` produces the keys and the signed root role; run it
once. The keys are the only thing that makes a runtime ours, so they belong
somewhere nothing that builds the application can read — a hardware token or a
KMS, which `tuftool` can sign from. Ship the root role as
`packaging/tuf/root.json`; until one is shipped, nothing is published to this
application and an install uses the recipe that shipped, which is what a machine
with no network does anyway.

`scripts/tuf-repo.sh build` publishes a targets directory as the next version.
It builds a whole repository beside the old one and swaps, so republishing
always repairs a served directory somebody has edited, and a failed publish
leaves what was working where it was.

Role expiries differ on purpose: root furthest out, because rotating it means
shipping an application; timestamp soonest, because that is what freshness
means — metadata nobody has re-signed lately stops being believed.

## Installation and activation

A version is installed at its final immutable path — `runtimes/<id>/<version>/`,
holding the recipe, the environment, the engine and the descriptor — and never
moved, because a Python environment remembers where it was built. What changes
when a version becomes the one that answers is a single row in the database.
There is no directory swap, no symlink, no moment with two active versions or
none; rollback is the same row pointed at the version kept from before, and the
store refuses to remove the active version outright.

The order is the point:

1. everything is written into the new version's own directory — recipe from the
   signed repository (or the one that ships, when the repository is
   unreachable), the environment built by `uv` *at that path*, the engine, the
   descriptor;
2. an engine is started **from that directory** and must answer the handshake
   with capabilities this application can use;
3. only then does the database mark it ready, and — separately — active.

A failure anywhere leaves whatever was answering untouched: the new directory
is debris, deleted on failure and swept at startup after a crash. A streamed
archive never appears at its destination until the whole of it has verified,
because TUF's client hands bytes over before the final digest is known and says
plainly not to use them if the stream then fails.

The interpreter is digest-pinned the way uv is, shared between versions keyed
by its pin, and refused rather than installed unpinned — it executes everything
else, and a hand-carried archive claims to be the same bytes, so it meets the
same pin. An install that predates versions is adopted where it lies rather
than broken by an application update. Which runtime version answered a session
is recorded with the session, so everything it produced can say what produced
it.

Offline is a priority order, not a fallback: what is installed and active keeps
answering, untouched by any network failure; a machine with nothing installed
installs the recipe that ships; the repository is consulted only to offer
something newer, and offering is not acting.

## Keys

`init` gives the root role three keys with a threshold of two — one lost is
recoverable, one stolen is not enough — and each other role one key of its own.
Keep the root keys apart, offline, and never anywhere that builds the
application. Timestamp's key is the one an automated re-signer holds, and it is
the least powerful on purpose: it can say a snapshot is current and cannot say
which targets are ours. `tuftool` signs from AWS KMS and SSM when the keys
should not exist as files at all.

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
