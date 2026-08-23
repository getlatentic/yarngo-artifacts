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

**Not yet trusted.** A recipe can name an archive holding a runtime's own
implementation, and it is verified against the digest the recipe gave. That
proves the bytes are the bytes the manifest named. It proves nothing about who
wrote the manifest — so anyone who can publish to the artifact repository can
name any archive and any digest.

While a recipe could only change which packages `uv` installs, that was a
tolerable amount of trust in the publishing account. Once it can deliver the
program itself it is not, so the path is closed: a runtime that publishes its
own engine is run by the one that ships, which is signed with the application.

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

## What comes next

1. Put the runtime archive behind TUF: the archive becomes a target, and its
   metadata is what says the target is current and authentic. Yarngo's own
   fields — runtime id, version, platform, engine API — travel beside the
   target rather than carrying the security.
2. Install to a staging directory, complete the handshake from staging, and
   promote atomically. A failure anywhere leaves the runtime that was working
   in place, and keeping the previous version is then rollback for free.
3. Give an installed runtime an immutable identity — id and version — and
   record it, so which runtime made a clip is answerable. `latest` is discovery;
   what got installed is a fact.
4. Separate the three version questions, which are not one: can this build read
   this metadata, can it talk to this engine, and which implementation is
   installed.

Only then is the archive path worth opening.

## The boundary of "a new runtime without a new application"

A runtime can be published without releasing the application when it fits
metadata this build can read, speaks an engine API this build implements, and
needs only capabilities this build already has. A runtime wanting streaming
output, several models at once, or a licence flow that Engine API 1 has no way
to express still needs the application to move. That is the normal shape of a
plugin contract, and it is worth saying out loud so the promise is not larger
than it is.
