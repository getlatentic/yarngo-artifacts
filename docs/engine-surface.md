# What the engine answers

The sidecar makes audio and manages models. It keeps no record of anything the
person has: the clips, the voices, the consent and the durable jobs are the
application's, in its SQLite database, and a method here that answered for them
would make this process a second place they live — the one the application
would then have to agree with.

One protocol, `yarngo-engine` version 1: JSON-RPC 2.0 over the child's stdio,
one message per line.

## Answered on the reader

Available while the model is loading or speaking, because none of them touch it.

| Method | What it is |
| --- | --- |
| `initialize` | the handshake, and what this engine can do |
| `ping` | whether it is there |
| `system_info` | what this machine is, including its disk |
| `model.install_status` | how far a download has got |
| `job.cancel` | ask an execution to stop, answered with what is true of it |

`job.cancel` is answered here rather than queued, which is the point of it: a
cancellation that waited its turn behind the work it was meant to stop would
arrive after it.

## Queued on the thread that owns the model

Sorted by what they touch rather than by how long they take — a fast call that
reads the model still waits for the slow one writing it.

| Method | What it is |
| --- | --- |
| `model.list` | the catalogue, with what is installed and resident |
| `model.load` | bring one into memory |
| `model.install` | start a download |
| `model.delete` | remove one's weights |
| `audio.prepare_reference` | cut the silence off a recording, at a given path, and measure it |
| `conditioning.prepare` | derive the speaker conditioning for a recording |
| `conditioning.invalidate` | forget every voice's conditioning |
| `synthesis.generate` | speak text into a given file |

## Saying what you can do

`initialize` answers with the protocol, its version, and `methods` — every
operation this runtime will answer, taken from the tables that answer them
rather than written out beside them. The application offers what that list
permits: a runtime without `audio.prepare_reference` is not asked to enrol a
voice, and the offer is withdrawn rather than taken and then refused after
somebody has spoken into a microphone.

## Being installed

A runtime is a directory with a `runtime.json` in it, under `runtimes/` in the
application's data directory. The one that ships is described the same way, in
`Resources/runtimes/`, so the path the application takes to its own engine is
the path a third one takes.

```json
{
  "id": "mlx",
  "name": "Apple silicon",
  "command": "{runtime}/mlx/.venv/bin/python",
  "args": ["{resources}/sidecar/engine.py"],
  "env": { "YARNGO_DATA": "{data}" }
}
```

`{data}` is everything the application keeps, `{runtime}` where installed
runtimes live, `{resources}` what shipped with the application, and `{self}` the
descriptor's own directory — which is what lets a runtime carry its own
interpreter without knowing where it will be installed.

A runtime can bring its own code. The published manifest names, per runtime, an
archive holding its implementation of this protocol and optionally the
descriptor that starts it. The archive is verified against the digest in the
manifest before anything is unpacked — it is code that will be executed,
arriving over a network — and lands in the runtime's own folder, which is what
`{self}` points at. A runtime that publishes a descriptor is the authority on
how it starts, and nothing writes over it.

Without one, a runtime is run by the copy of the engine that ships with the
application. That is what the first runtime does today, and what any of them
falls back to when the manifest cannot be reached.

Nothing about a runtime ships with the application — an interpreter and its
speech packages are hundreds of megabytes and update on their own cadence — so a
new installation has none and is shown what it can fetch for this machine. The
installer writes the descriptor as its last step, which is what turns a
downloaded interpreter into a runtime; one installed before descriptors existed
is described on the next start rather than needing to be installed again.

Shipped runtimes are read first, and a name is not a claim on it: an installed
runtime calling itself `mlx` does not quietly become the engine that starts. A
descriptor names a program to run, which is the same trust as a language server
or an editor extension — what it can do is what the person running it can do.

## Two rules the surface follows

**Nothing takes an identifier the engine would have to look up.** Conditioning
and synthesis are given the recording and the transcript; there is no voice
table here to consult, which is what stops one growing back.

**The caller names every path.** The engine writes where it is told. That is
what lets a deletion know every file it has to find, a restart know which files
it may remove, and two attempts at the same clip not write over each other.

## What it says while it works

`job.progress` every quarter second during a generation, carrying the job and
execution it belongs to. Coalesced per execution by the writer, so a subscriber
that has fallen behind sees the latest rather than all of them.

`job.cancelled` when an execution stops, saying whether it had started —
stopped before beginning and stopped part way are different facts, and the job
waiting to hear how it ended needs the difference.
