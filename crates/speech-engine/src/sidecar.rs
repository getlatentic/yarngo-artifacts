//! MLX backend, driven as a child process speaking line-delimited JSON.
//!
//! A sidecar rather than an embedded interpreter: the model stack is Python, and
//! a separate process means a crash inside inference cannot take the UI down
//! with it. The protocol is one JSON object per line so it can be exercised by
//! hand with `echo ... | python engine.py` when something misbehaves.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::{
    Capabilities, Clip, DiskSpace, EngineError, InstallStatus, ModelSpec, Result, SpeechEngine,
    Synthesis, SynthesisRequest, SystemInfo, Voice,
};

pub struct MlxSidecar {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl MlxSidecar {
    /// Spawn the sidecar. `python` is the interpreter of the environment holding
    /// the model stack; `script` is `sidecar/engine.py`.
    pub fn spawn(python: &Path, script: &Path, working_dir: &Path) -> Result<Self> {
        let mut command = Command::new(python);
        command.arg(script).current_dir(working_dir);
        // A GUI process spawning python.exe flashes a console window on
        // Windows unless told not to. Written from the documented flag,
        // compiled here, not yet run there.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            // Told, not inferred. Both sides used to derive the data directory
            // from their own environment under different variable names, so
            // overriding one moved the app without moving its storage.
            .env("YARNGO_DATA", crate::paths::data_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| EngineError::Transport(format!("could not start sidecar: {e}")))?;

        let stdin = child.stdin.take().ok_or(EngineError::NotRunning)?;
        let stdout = child.stdout.take().ok_or(EngineError::NotRunning)?;

        let mut engine = Self { child, stdin, stdout: BufReader::new(stdout), next_id: 1 };
        // Fail fast if the environment is wrong, rather than at first synthesis.
        let _: Value = engine.call("ping", json!({}))?;
        Ok(engine)
    }

    fn call<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> Result<T> {
        let id = self.next_id;
        self.next_id += 1;

        let request = json!({ "id": id, "method": method, "params": params });
        // A dead child shows up as EOF on read, but writing to it first can
        // fail with a broken pipe. Both mean the same thing to the caller, and
        // only one of them triggers a restart, so they are reported alike.
        writeln!(self.stdin, "{request}")
            .and_then(|_| self.stdin.flush())
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::BrokenPipe => EngineError::NotRunning,
                _ => EngineError::Transport(e.to_string()),
            })?;

        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .map_err(|e| EngineError::Transport(e.to_string()))?;
        if read == 0 {
            return Err(EngineError::NotRunning);
        }

        let response: Value = serde_json::from_str(&line)
            .map_err(|e| EngineError::Transport(format!("unparseable reply: {e}")))?;

        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            let message = response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(EngineError::Rejected(message.to_string()));
        }

        serde_json::from_value(response.get("result").cloned().unwrap_or(Value::Null))
            .map_err(|e| EngineError::Transport(format!("unexpected result shape: {e}")))
    }
}

impl Drop for MlxSidecar {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(serde::Deserialize)]
struct ModelsReply {
    models: Vec<ModelSpec>,
}

#[derive(serde::Deserialize)]
struct FreedReply {
    #[serde(default)]
    freed_bytes: u64,
}

#[derive(serde::Deserialize)]
struct PrepareReply {
    prepared_s: f32,
}

#[derive(serde::Deserialize)]
struct VoicesReply {
    voices: Vec<Voice>,
}

#[derive(serde::Deserialize)]
struct ClipsReply {
    clips: Vec<Clip>,
}

impl SpeechEngine for MlxSidecar {
    fn capabilities(&self) -> Capabilities {
        Capabilities { cloning: true, streaming: false }
    }

    fn models(&mut self, refresh: bool) -> Result<Vec<ModelSpec>> {
        let reply: ModelsReply = self.call("list_models", json!({ "refresh": refresh }))?;
        Ok(reply.models)
    }

    fn register_voice(&mut self, voice: &Voice) -> Result<()> {
        let _: Value = self.call(
            "register_voice",
            json!({
                "voice_id": voice.voice_id,
                "label": voice.label,
                "reference_audio": voice.reference_audio,
                "reference_text": voice.reference_text,
                "consent_statement": voice.consent.statement,
                "app_version": voice.consent.app_version,
                "source": voice.consent.source,
            }),
        )?;
        Ok(())
    }

    fn voices(&mut self) -> Result<Vec<Voice>> {
        let reply: VoicesReply = self.call("list_voices", json!({}))?;
        Ok(reply.voices)
    }

    fn delete_voice(&mut self, voice_id: &str) -> Result<()> {
        let _: Value = self.call("delete_voice", json!({ "voice_id": voice_id }))?;
        Ok(())
    }

    fn rename_voice(&mut self, voice_id: &str, label: &str) -> Result<Vec<Voice>> {
        let reply: VoicesReply =
            self.call("rename_voice", json!({ "voice_id": voice_id, "label": label }))?;
        Ok(reply.voices)
    }

    fn delete_model(&mut self, model: &str) -> Result<u64> {
        let reply: FreedReply = self.call("delete_model", json!({ "model": model }))?;
        Ok(reply.freed_bytes)
    }

    fn prepare_voice(&mut self, voice_id: &str, model: Option<&str>) -> Result<f32> {
        let reply: PrepareReply =
            self.call("prepare_voice", json!({ "voice_id": voice_id, "model": model }))?;
        Ok(reply.prepared_s)
    }

    fn install_model(&mut self, model: &str) -> Result<InstallStatus> {
        self.call("install_model", json!({ "model": model }))
    }

    fn install_status(&mut self, model: &str) -> Result<InstallStatus> {
        self.call("install_status", json!({ "model": model }))
    }

    fn disk_space(&mut self) -> Result<DiskSpace> {
        self.call("disk_free", json!({}))
    }

    fn clips(&mut self) -> Result<Vec<Clip>> {
        let reply: ClipsReply = self.call("list_clips", json!({}))?;
        Ok(reply.clips)
    }

    fn delete_clip(&mut self, clip_id: &str) -> Result<Vec<Clip>> {
        let reply: ClipsReply = self.call("delete_clip", json!({ "clip_id": clip_id }))?;
        Ok(reply.clips)
    }

    fn rename_clip(&mut self, clip_id: &str, name: &str) -> Result<Vec<Clip>> {
        let reply: ClipsReply =
            self.call("rename_clip", json!({ "clip_id": clip_id, "name": name }))?;
        Ok(reply.clips)
    }

    fn duplicate_clip(&mut self, clip_id: &str) -> Result<Vec<Clip>> {
        let reply: ClipsReply = self.call("duplicate_clip", json!({ "clip_id": clip_id }))?;
        Ok(reply.clips)
    }

    fn system_info(&mut self) -> Result<SystemInfo> {
        self.call("system_info", json!({}))
    }

    fn ping(&mut self) -> Result<()> {
        let _: Value = self.call("ping", json!({}))?;
        Ok(())
    }

    /// By file, because this backend is not reading its input while it works:
    /// one request and one reply over a pipe, and the request in flight is the
    /// generation being asked to stop.
    fn cancel_generation(&mut self) -> Result<()> {
        crate::runtime::request_cancel();
        Ok(())
    }

    fn progress(&mut self) -> Option<crate::runtime::Generating> {
        crate::runtime::read_progress()
    }

    fn synthesize(&mut self, request: &SynthesisRequest) -> Result<Synthesis> {
        self.call(
            "synthesize",
            json!({
                "text": request.text,
                "output": request.output,
                "model": request.model,
                "voice_id": request.voice_id,
                "seed": request.seed,
                "name": request.name,
                "clip_id": request.clip_id,
            }),
        )
    }
}

/// Locations the app looks for its sidecar, so a dev checkout and a bundled
/// app can use the same code path.
pub fn default_script(root: &Path) -> PathBuf {
    root.join("sidecar").join("engine.py")
}
