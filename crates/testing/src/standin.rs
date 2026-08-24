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
    limited(dir, behaviour, &[])
}

/// The same, with some of it taken away — a runtime that does not do
/// everything, which is the case capability negotiation exists for and the one
/// a real engine cannot be asked to be.
pub fn limited(dir: &Path, behaviour: &str, without: &[&str]) -> PathBuf {
    heard_up_to(dir, behaviour, without, -1)
}

/// The same, with a stand-in for listening back to the reference: `heard_words`
/// is how many words of the script this recording will be treated as
/// containing, and `-1` is all of them. What makes "the reader stopped early"
/// something a test can produce without a speech-recognition model.
pub fn heard_up_to(
    dir: &Path,
    behaviour: &str,
    without: &[&str],
    heard_words: i32,
) -> PathBuf {
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
    prepared = {{"output_path": out, "seconds": round(os.path.getsize(out) / 48000.0, 2),
                "trimmed_lead_s": 0.0, "trimmed_tail_s": 0.0}}
    # A stand-in for listening back. `heard_words` says how much of the script
    # this recording is to be treated as containing, so a test can produce the
    # reader who stopped early without needing a model.
    script = params.get("script")
    if script:
        limit = {heard_words}
        words = script.split()
        prepared["text"] = " ".join(words if limit < 0 else words[:limit])
    return prepared

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
for name in {without:?}:
    MODEL.pop(name, None)
    BROKER.pop(name, None)
protocol.serve(broker=BROKER, model=MODEL, capabilities={{"backend": "stand-in"}})
"#,
        dir = sidecar_dir().to_string_lossy(),
        wave = WAVE,
        behaviour = behaviour,
        without = without,
        heard_words = heard_words,
    );
    let path = dir.join("standin_engine.py");
    std::fs::write(&path, program).expect("write the stand-in engine");
    path
}

/// The interpreter to run it with. Nothing here needs the model stack.
pub fn python() -> PathBuf {
    PathBuf::from("/usr/bin/python3")
}

/// The version name every stand-in install wears. One is enough: these tests
/// are about the engine's behaviour, not about upgrades.
pub const VERSION: &str = "test";

/// Lay out a stand-in runtime the way an install leaves one: a version
/// directory holding the engine, the descriptor, and an environment with an
/// interpreter in it — so a test goes through the same loading and the same
/// validation the application does, rather than constructing a descriptor no
/// file ever had to satisfy.
///
/// The database rows that make it *the* runtime are the caller's to write,
/// because they live in the application's store and this crate stays beneath
/// it.
pub fn install(root: &Path, id: &str, python: &Path, behaviour: &str, without: &[&str]) {
    install_hearing(root, id, python, behaviour, without, -1)
}

/// The same, with the stand-in told how much of a reference script its
/// recordings contain.
pub fn install_hearing(
    root: &Path,
    id: &str,
    python: &Path,
    behaviour: &str,
    without: &[&str],
    heard_words: i32,
) {
    let home = home(root, id);
    std::fs::create_dir_all(&home).expect("runtime directory");
    let script = heard_up_to(&home, behaviour, without, heard_words);
    std::fs::rename(&script, home.join("engine.py")).expect("engine.py");

    // The interpreter inside the version's own environment, linked rather
    // than copied: what matters is that it is where the descriptor points.
    let bin = home.join(".venv").join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let linked = bin.join("python3");
    let _ = std::fs::remove_file(&linked);
    #[cfg(unix)]
    std::os::unix::fs::symlink(python, &linked).expect("interpreter");

    std::fs::write(
        home.join("runtime.json"),
        serde_json::json!({
            "schema": 1,
            "id": id,
            "name": format!("Stand-in {id}"),
            "engine": "own",
            "program": "{venv}/bin/python3",
            "arguments": ["{engine}"],
        })
        .to_string(),
    )
    .expect("descriptor");
}

/// Where [`install`] puts the runtime's version directory.
pub fn home(root: &Path, id: &str) -> PathBuf {
    root.join("runtimes").join(id).join(VERSION)
}

/// The stand-in resolves its marker and its records beside itself, which is
/// the version directory once it is installed. Named here so a test does not
/// have to know that layout.
pub fn beside(root: &Path, id: &str, name: &str) -> PathBuf {
    home(root, id).join(name)
}
