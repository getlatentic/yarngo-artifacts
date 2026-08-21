# Sidecar v1: what each method touches

Every method the sidecar answers today, what it reads and writes, and where it
should live once Rust owns durable state. Extracted from `sidecar/engine.py`
and the Rust call chain, not from memory — an earlier hand-written pass lost
three methods.

Call chain: an app call site reaches `EngineHandle` (`crates/speech-engine/src/handle.rs`),
which serialises every request onto one worker thread, which calls the trait
method in `crates/speech-engine/src/sidecar.rs`, which writes one JSON line and
blocks on the reply.

## Current behaviour

| method | app call sites | reads | writes | locks | blocks on | progress / cancel |
|---|---|---|---|---|---|---|
| `ping` | *(handshake in `spawn`)* | — | — | — | nothing | — |
| `list_models` | `models.rs:231` | `MODELS`, `_models`, size cache | size cache | — | network, disk | — |
| `load_model` | **none** | `MODELS` | `_models` | — | model | — |
| `register_voice` | `main.rs:603` | `MODELS`, `VOICE_DIR` | `_voices`, `voices.json`, WAV | — | model, disk | — |
| `list_voices` | `main.rs:326,614,1386` | `_voices` | — | — | nothing | — |
| `delete_voice` | `main.rs:1385`, `settings.rs:681` | `_voices`, `_models` | `_voices`, WAV, prompt cache | prompt cache | disk | — |
| `rename_voice` | `main.rs:1367` | `_voices` | `_voices`, `voices.json` | — | disk | — |
| `synthesize` | `main.rs:739` | `MODELS`, `_models`, `_CANCEL` | WAV, `clips.json`, `_PROGRESS` | — | model, disk | **both by file** |
| `prepare_voice` | `main.rs:376` | `_voices`, `MODELS` | prompt cache | prompt cache | model | — |
| `list_clips` | `main.rs:1502` | `clips.json` | — | — | disk | — |
| `rename_clip` | `clips.rs:284,297` | `clips.json` | `clips.json` | — | disk | — |
| `duplicate_clip` | `workspace.rs:588`, `main.rs:1016` | `clips.json`, WAV | `clips.json`, WAV | — | disk | — |
| `delete_clip` | `main.rs:1008,1517` | `clips.json` | `clips.json`, WAV | — | disk | — |
| `install_model` | `switcher.rs:165`, `models.rs:215`, `main.rs:1239`, `settings.rs:381` | `MODELS` | `_installs` | `_installs_lock` | network *(on a thread)* | **status polled** |
| `install_status` | `main.rs:1254` | `_installs`, `MODELS` | — | `_installs_lock` | network | *(is the poll)* |
| `delete_model` | `main.rs:1308`, `settings.rs:316` | `MODELS`, `_models` | `_models`, model cache | — | disk | — |
| `disk_free` | **none** | `VOICE_DIR` | — | — | nothing | — |
| `system_info` | `main.rs:1486` | host | — | — | nothing | — |

Three methods have no consumer. `load_model` is never called from Rust at all —
loading happens implicitly inside `_load()` on the synthesis and conditioning
paths. `disk_free` is unreferenced through the whole Rust stack; the Storage
pane walks directories itself in `crates/app/src/storage.rs`. `ping` is called
only by `MlxSidecar::spawn` as a startup handshake and has no trait method.

Only `synthesize` carries progress or cancellation, and both travel by file
(`generating.json`, `cancel`) because the sidecar handles one request at a time
and is not reading stdin while it works.

## Target ownership

| method | owner | replacement | kind |
|---|---|---|---|
| `ping` | Python | `engine.ping` | immediate |
| `list_models` | split | Rust reads the catalogue and what is installed; `engine.capabilities` reports runtime compatibility | immediate |
| `load_model` | Python | `engine.load` | async, ephemeral |
| `register_voice` | split | Rust writes the voice revision and consent; `voice.condition` derives conditioning | async, ephemeral compute over durable inputs |
| `list_voices` | Rust | SQLite query, merged with engine readiness | immediate |
| `delete_voice` | Rust | durable `VoiceDelete` job; `conditioning.invalidate` on the engine | async, durable |
| `rename_voice` | Rust | SQLite transaction | immediate |
| `synthesize` | split | Rust owns the durable job; `synthesis.submit` executes it | async, durable |
| `prepare_voice` | Python | `voice.condition` | async, ephemeral |
| `list_clips` | Rust | SQLite query | immediate |
| `rename_clip` | Rust | SQLite transaction | immediate |
| `duplicate_clip` | Rust | SQLite transaction; asset-sharing semantics undecided | immediate |
| `delete_clip` | Rust | SQLite tombstone plus asset reconciliation | async, durable |
| `install_model` | split | Rust owns the durable job; Python fetches | async, durable |
| `install_status` | Rust | `job.get` / `job.list_active`, alongside `job.progress` events | immediate |
| `delete_model` | split | Rust owns the durable job; Python unloads and removes | async, durable |
| `disk_free` | Rust | delete; Rust already measures its own directories | — |
| `system_info` | split | Rust reports the host; `engine.capabilities` reports the backend | immediate |

Seven methods leave the protocol entirely. Two are already dead. `install_status`
becomes generic queryable job state rather than a model-specific poll, because
events alone cannot reconcile across a restart on either side.

## What already exists to build on

`crates/speech-engine/tests/protocol.rs` drives a stand-in sidecar written per
test — a Python script that speaks the wire however the case needs. The
contract tests for v2 extend that rather than starting over.
