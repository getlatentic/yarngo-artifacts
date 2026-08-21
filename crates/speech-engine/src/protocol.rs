//! The Yarngo engine API: JSON-RPC 2.0, one object per line, over the sidecar's
//! stdio.
//!
//! The wire is not ours. Correlating a reply to its request by id regardless of
//! arrival order, sending a notification that expects no reply, and reporting a
//! fault in a defined shape are all JSON-RPC 2.0; writing our own version of
//! them would only mean getting them subtly wrong. What is ours is the methods,
//! and the concurrency underneath — which is what the specification is silent
//! about.
//!
//! The previous version wrote a line and read the next one back, which made
//! every reply the answer to the last question asked. Nothing else could
//! arrive, so progress and cancellation had to travel by file — the only
//! channel that stayed open to a process that was busy — and every cheap call
//! queued behind whatever long one was running.
//!
//! Here a reader thread owns the child's output and sorts what comes off it.
//! Anything carrying an `id` is a reply and goes to whoever is waiting for that
//! id; anything without one is an event and goes to the subscriber. A writer
//! thread owns the input, so two callers cannot interleave halves of a line.
//!
//! Both threads outlive individual requests, which is the point: the engine can
//! speak while it works, and a cheap call can be answered while an expensive one
//! is still running. Model operations do not overtake each other — the sidecar
//! runs those one at a time — so what arrives out of order is a broker reply
//! ahead of a model reply asked for first.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{EngineError, Result};

pub const JSONRPC_VERSION: &str = "2.0";

/// What this speaks, named so a peer can be told apart from anything else that
/// is also JSON-RPC over a pipe. The wire is a standard; the methods are ours.
pub const PROTOCOL_NAME: &str = "yarngo-engine";

/// The version of those methods. One because nothing has shipped — there is no
/// earlier version to be compatible with, and carrying a number that suggests
/// otherwise would invite compatibility work nobody owes.
pub const PROTOCOL_VERSION: u32 = 1;

/// JSON-RPC's own codes, for faults in the exchange itself.
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;

    /// Ours, in the range the specification leaves to the server.
    pub const ENGINE_BUSY: i64 = -32001;
    pub const VOICE_DELETED: i64 = -32002;
    pub const MODEL_NOT_INSTALLED: i64 = -32003;
    pub const JOB_ALREADY_TERMINAL: i64 = -32004;
}

/// How many messages may be queued before a producer is made to wait, and how
/// much progress may pile up before it is dropped instead.
const QUEUE_DEPTH: usize = 256;

/// The longest line this will act on. A guard rather than a hard bound: the
/// line has already been read to find its end, so this stops us parsing what
/// arrived, not receiving it.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Something the engine said without being asked.
#[derive(Clone, Debug)]
pub struct Event {
    pub method: String,
    pub params: Value,
}

/// The events one connection has sent, and the only place they are consumed.
///
/// The queue behind this is unbounded, which is deliberate and is the whole
/// reason it exists. The reader thread must never block: everything it delivers
/// shares a thread with every reply it routes, so waiting for a slow subscriber
/// would stall replies the subscriber may itself be waiting on — a deadlock
/// this had before a test found it. Depth is instead held down by dropping
/// progress, which is the same fact at successive values. Nothing else is
/// dropped, and nothing else is frequent.
pub struct Events {
    incoming: Receiver<Event>,
    queued: Arc<AtomicUsize>,
}

impl Events {
    pub fn recv_timeout(&self, timeout: Duration) -> std::result::Result<Event, RecvTimeoutError> {
        let event = self.incoming.recv_timeout(timeout)?;
        self.queued.fetch_sub(1, Ordering::SeqCst);
        Ok(event)
    }

    pub fn try_recv(&self) -> Option<Event> {
        let event = self.incoming.try_recv().ok()?;
        self.queued.fetch_sub(1, Ordering::SeqCst);
        Some(event)
    }
}

/// An error the engine reported, rather than one the transport produced.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

type Waiting = SyncSender<std::result::Result<Value, RpcError>>;

/// Requests still awaiting a reply, by id.
///
/// Shared with the reader thread, which is the only thing that removes from it
/// — either because the reply arrived or because the child stopped and every
/// caller has to be told at once.
type Pending = Arc<Mutex<HashMap<u64, Waiting>>>;

/// A request the engine has been given and has not answered.
///
/// Holding one costs nothing and blocks nothing. Dropping one abandons the
/// reply, which the reader will then have nowhere to put and will discard.
pub struct Outstanding {
    id: u64,
    method: String,
    waiting: Receiver<std::result::Result<Value, RpcError>>,
    pending: Pending,
}

impl Outstanding {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn wait(self, timeout: Duration) -> Result<Value> {
        match self.waiting.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(EngineError::Rejected(error.to_string())),
            // The reader drops every waiting sender when the child ends, which
            // arrives here as a closed channel rather than as a timeout.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(EngineError::NotRunning),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.pending.lock().expect("pending").remove(&self.id);
                Err(EngineError::Transport(format!(
                    "{} did not answer within {timeout:?}",
                    self.method
                )))
            }
        }
    }
}

pub struct Connection {
    child: Child,
    outgoing: SyncSender<String>,
    pending: Pending,
    next_id: AtomicU64,
    /// Set once the child's output ends. Read before waiting on anything, so a
    /// caller arriving after the end fails immediately rather than on a timeout.
    ended: Arc<AtomicBool>,
    /// Events dropped because the subscriber was too slow. Progress is the only
    /// kind that may be dropped, and this is how often it happened.
    dropped: Arc<AtomicU64>,
    /// Lines that were not frames. On this channel that means something is
    /// writing where the protocol lives — a library's progress bar, a stray
    /// print — and one such line is enough to break a reply. Counted so the
    /// condition is observable rather than merely survivable.
    malformed: Arc<AtomicU64>,
}

impl Connection {
    /// Take over a spawned child's pipes and start the two threads.
    ///
    /// The event receiver is handed out rather than kept: it is not `Sync`, and
    /// a connection that cannot be shared between threads could only ever be
    /// asked one thing at a time — which is what version 1 already did.
    pub fn attach(
        mut child: Child,
        stdin: ChildStdin,
        stdout: ChildStdout,
        stderr: Option<ChildStderr>,
    ) -> (Self, Events) {
        let pending: Pending = Arc::default();
        let ended = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let malformed = Arc::new(AtomicU64::new(0));
        let (outgoing, to_write) = sync_channel::<String>(QUEUE_DEPTH);
        let (event_tx, incoming) = channel::<Event>();
        let queued = Arc::new(AtomicUsize::new(0));

        spawn_writer(stdin, to_write);
        spawn_reader(
            stdout,
            pending.clone(),
            event_tx,
            queued.clone(),
            ended.clone(),
            dropped.clone(),
            malformed.clone(),
        );
        // Drained rather than merely piped: a child that fills an unread stderr
        // pipe blocks on its own logging, which looks like a hung engine.
        if let Some(stderr) = stderr {
            spawn_stderr_drain(stderr);
        }

        let _ = child.try_wait();
        let connection = Self {
            child,
            outgoing,
            pending,
            next_id: AtomicU64::new(1),
            ended,
            dropped,
            malformed,
        };
        (connection, Events { incoming, queued })
    }

    /// The opening exchange. Establishes that the other end is this engine, and
    /// this version of it, before anything depends on a method's shape.
    pub fn initialize(&self, timeout: Duration) -> Result<Value> {
        let reply = self.request(
            "initialize",
            json!({ "protocol": PROTOCOL_NAME, "version": PROTOCOL_VERSION }),
            timeout,
        )?;
        match reply.get("protocol").and_then(Value::as_str) {
            Some(PROTOCOL_NAME) => {}
            Some(other) => {
                return Err(EngineError::Rejected(format!(
                    "the other end speaks {other:?}, not {PROTOCOL_NAME:?}"
                )))
            }
            None => {
                return Err(EngineError::Transport(
                    "the other end did not say what it speaks".into(),
                ))
            }
        }
        match reply.get("version").and_then(Value::as_u64) {
            Some(theirs) if theirs as u32 == PROTOCOL_VERSION => Ok(reply),
            Some(theirs) => Err(EngineError::Rejected(format!(
                "sidecar speaks {PROTOCOL_NAME} {theirs}, this build speaks {PROTOCOL_VERSION}"
            ))),
            None => Err(EngineError::Transport(
                "sidecar did not state a protocol version".into(),
            )),
        }
    }

    /// Ask, and wait for the reply with this request's id — not for the next
    /// line to arrive, which may belong to someone else or to no one.
    /// Write a request and return without waiting for it.
    ///
    /// The reply arrives on the returned handle whenever it arrives, and other
    /// requests can be written and answered in the meantime — which is the
    /// whole reason this protocol carries ids. A caller that waits here instead
    /// of holding the handle turns a multiplexed connection back into a queue.
    pub fn send(&self, method: &str, params: Value) -> Result<Outstanding> {
        if self.ended.load(Ordering::SeqCst) {
            return Err(EngineError::NotRunning);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (answer, waiting) = sync_channel(1);
        self.pending.lock().expect("pending").insert(id, answer);

        let line = json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();
        if self.outgoing.send(line).is_err() {
            self.pending.lock().expect("pending").remove(&id);
            return Err(EngineError::NotRunning);
        }
        Ok(Outstanding {
            id,
            method: method.to_string(),
            waiting,
            pending: self.pending.clone(),
        })
    }

    /// Write a request and wait for its reply. For callers with nothing to do
    /// in between.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.send(method, params)?.wait(timeout)
    }

    /// Progress events discarded because nothing was reading them.
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::SeqCst)
    }

    /// Lines received that were not protocol frames.
    pub fn malformed_lines(&self) -> u64 {
        self.malformed.load(Ordering::SeqCst)
    }

    /// End the process, now.
    ///
    /// Not a request. Where this is used the question is whether the process
    /// can still do something it has been told to stop doing, and one that has
    /// been asked and has not answered is one that still can. Waits, so that a
    /// caller told the engine is gone is not told it before it is.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()?;
        self.child.wait().map(|_| ())
    }

    pub fn has_ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_writer(mut stdin: ChildStdin, lines: Receiver<String>) {
    std::thread::spawn(move || {
        // One writer, so two callers cannot interleave halves of a line.
        for line in lines {
            if writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_err() {
                break;
            }
        }
    });
}

fn spawn_stderr_drain(stderr: ChildStderr) {
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
            eprintln!("sidecar: {line}");
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_reader(
    stdout: ChildStdout,
    pending: Pending,
    events: Sender<Event>,
    queued: Arc<AtomicUsize>,
    ended: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    malformed: Arc<AtomicU64>,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.len() > MAX_FRAME_BYTES {
                malformed.fetch_add(1, Ordering::SeqCst);
                eprintln!("sidecar: frame exceeds the size limit ({} bytes)", line.len());
                continue;
            }
            let Ok(message) = serde_json::from_str::<Value>(line) else {
                // Not a frame at all.
                // Not a frame. Reported rather than ignored: on this channel it
                // means something is writing where the protocol lives.
                malformed.fetch_add(1, Ordering::SeqCst);
                eprintln!("sidecar: unparseable protocol line: {line}");
                continue;
            };
            if message.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
                malformed.fetch_add(1, Ordering::SeqCst);
                eprintln!("sidecar: message is not JSON-RPC {JSONRPC_VERSION}: {line}");
                continue;
            }
            route(message, &pending, &events, &queued, &dropped);
        }
        // Output ended, so nothing else is coming and nobody should keep
        // waiting. Dropping each sender wakes its caller with a closed channel.
        ended.store(true, Ordering::SeqCst);
        pending.lock().expect("pending").clear();
    });
}

fn route(
    message: Value,
    pending: &Pending,
    events: &Sender<Event>,
    queued: &Arc<AtomicUsize>,
    dropped: &Arc<AtomicU64>,
) {
    match message.get("id").and_then(Value::as_u64) {
        Some(id) => {
            // Removed, so a duplicate reply for the same id finds nothing and
            // cannot answer a caller twice.
            let Some(waiting) = pending.lock().expect("pending").remove(&id) else {
                eprintln!("sidecar: reply to unknown request {id}");
                return;
            };
            let answer = match message.get("error") {
                Some(error) => match serde_json::from_value::<RpcError>(error.clone()) {
                    Ok(error) => Err(error),
                    Err(_) => Err(RpcError {
                        code: -1,
                        message: error.to_string(),
                        data: None,
                    }),
                },
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = waiting.send(answer);
        }
        None => {
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                eprintln!("sidecar: message with neither id nor method");
                return;
            };
            let event = Event {
                method: method.to_string(),
                params: message.get("params").cloned().unwrap_or(Value::Null),
            };
            deliver(event, events, queued, dropped);
        }
    }
}

/// Hand an event to the subscriber without ever waiting for it.
///
/// Progress may be dropped once the backlog reaches `QUEUE_DEPTH`: it is the
/// same fact at successive values, and the newest one supersedes what is
/// already queued. Everything else — a job finishing, failing, being cancelled
/// — is said once and is kept, because a lost terminal event leaves a job that
/// never resolves.
fn deliver(
    event: Event,
    events: &Sender<Event>,
    queued: &Arc<AtomicUsize>,
    dropped: &Arc<AtomicU64>,
) {
    let coalescible = event.method == "job.progress";
    if coalescible && queued.load(Ordering::SeqCst) >= QUEUE_DEPTH {
        dropped.fetch_add(1, Ordering::SeqCst);
        return;
    }
    queued.fetch_add(1, Ordering::SeqCst);
    if events.send(event).is_err() {
        queued.fetch_sub(1, Ordering::SeqCst);
    }
}
