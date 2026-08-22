//! An engine that can be made to misbehave.
//!
//! The real one cannot be asked to fail, to ignore a cancellation, or to refuse
//! to start — and those are exactly the cases the durable path exists for. This
//! is `sidecar/protocol.py`, the real serving layer, with a stand-in where the
//! model would be: so what is under test is everything except the inference,
//! and the parts that are under test are the real ones.
//!
//! Written to a file rather than passed as `-c`, because the application spawns
//! its engine by path and a stand-in that could not be spawned the same way
//! would be testing a different arrangement.

use std::path::{Path, PathBuf};

/// The repository's `sidecar` directory, found from this crate.
pub fn sidecar_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sidecar")
}

/// A wave file of `seconds` at 24 kHz, as Python source.
const WAVE: &str = r#"
def wave(seconds, rate=24000):
    frames = int(seconds * rate)
    data = b"\0\0" * frames
    return (b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVEfmt " +
            struct.pack("<IHHIIHH", 16, 1, 1, rate, rate * 2, 2, 16) +
            b"data" + struct.pack("<I", len(data)) + data)
"#;

/// Write a stand-in engine. `behaviour` is Python run inside
/// `synthesis.generate` before it writes, indented by four spaces.
///
/// A file named `refuse` beside the script makes the next start fail, which is
/// how a replacement that will not come up is arranged. Beside the script and
/// not in the environment, because the environment belongs to the whole test
/// process and one test's engine must not refuse to start because another one
/// is testing that.
pub fn script(dir: &Path, behaviour: &str) -> PathBuf {
    let program = format!(
        r#"
import sys, os, time, json, struct, threading
HERE = os.path.dirname(os.path.abspath(__file__))
refuse = os.path.join(HERE, "refuse")
if os.path.exists(refuse):
    print("the stand-in was told not to start", file=sys.stderr)
    raise SystemExit(1)
sys.path.insert(0, {dir:?})
import protocol
{wave}

def generate(params, ctx):
    seconds = float(params.get("seed") or 2000) / 1000.0
    # Said as soon as the work starts, as the real engine does: an application
    # that hears nothing until the end cannot tell working from stuck.
    ctx.emit("job.progress", {{"chunks_done": 0, "chunks": 1,
                              "written_s": 0.0, "elapsed_s": 0.0}})
{behaviour}
    ctx.emit("job.progress", {{"chunks_done": 1, "chunks": 1,
                              "written_s": round(seconds, 2), "elapsed_s": 0.1}})
    path = params["output_path"]
    os.makedirs(os.path.dirname(path), exist_ok=True)
    # What it was actually asked for, so a test can check what reached the
    # engine rather than what the caller believed it sent. Beside the script and
    # not beside the output: the staging directory holds what the application
    # put there, and a test that leaves something in it is testing its own mess.
    with open(os.path.join(HERE, "generate.asked.json"), "w") as asked:
        json.dump(params, asked)
    with open(path, "wb") as handle:
        handle.write(wave(seconds))
    return {{"output_path": path, "audio_s": round(seconds, 2), "gen_s": 0.1,
             "seed": params.get("seed"), "sample_rate": 24000, "chunks": 1}}

def prepare_reference(params, ctx):
    source, out = params["source"], params["output_path"]
    os.makedirs(os.path.dirname(out), exist_ok=True)
    if os.path.abspath(source) != os.path.abspath(out):
        with open(source, "rb") as reading, open(out, "wb") as writing:
            writing.write(reading.read())
    # Measured from the bytes rather than invented: a stand-in for the audio
    # stack should still answer from the file it was given.
    return {{"output_path": out, "seconds": round(os.path.getsize(out) / 48000.0, 2),
             "trimmed_lead_s": 0.0, "trimmed_tail_s": 0.0}}

def condition(params, ctx):
    with open(os.path.join(HERE, "conditioning.asked.json"), "w") as asked:
        json.dump(params, asked)
    return {{"prepared_s": 0.01}}

def invalidate(params, ctx):
    cleared = getattr(invalidate, "held", 1)
    invalidate.held = 0
    return {{"entries_removed": cleared, "scope_applied": "all",
             "status": "cleared" if cleared else "already_empty"}}

BROKER = {{"ping": lambda params: {{"pong": True}},
          "system_info": lambda params: {{"os": "stand-in", "chip": "stand-in",
                                         "memory_bytes": 1, "free_bytes": 1,
                                         "total_bytes": 2, "data_dir": "."}}}}
MODEL = {{"synthesis.generate": generate,
         "audio.prepare_reference": prepare_reference,
         "conditioning.prepare": condition,
         "conditioning.invalidate": invalidate,
         "model.list": lambda params, ctx: {{"models": []}}}}
protocol.serve(broker=BROKER, model=MODEL, capabilities={{"backend": "stand-in"}})
"#,
        dir = sidecar_dir().to_string_lossy(),
        wave = WAVE,
        behaviour = behaviour,
    );
    let path = dir.join("standin_engine.py");
    std::fs::write(&path, program).expect("write the stand-in engine");
    path
}

/// The interpreter to run it with. Nothing here needs the model stack.
pub fn python() -> PathBuf {
    PathBuf::from("/usr/bin/python3")
}
