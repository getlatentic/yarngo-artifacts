//! The wire, version 2: correlated replies and unsolicited events.
//!
//! Version 1 wrote a line and read the next one back, which made every reply
//! the answer to the last question asked. Nothing else could arrive, so
//! progress and cancellation had to travel by file — the only channel that
//! stayed open to a process that was busy — and every cheap call queued behind
//! whatever long one was running.
//!
//! Here a reader thread owns the child's output and sorts what comes off it.
//! Anything carrying an `id` is a reply and goes to whoever is waiting for that
//! id; anything without one is an event and goes to the subscriber. A writer
//! thread owns the input, so two callers cannot interleave halves of a line.
//!
//! Both threads outlive individual requests, which is the point: the engine can
//! speak while it works, and a request can be answered out of order.

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

/// The version this build speaks. The sidecar states its own during the
/// opening exchange, and a disagreement stops the connection there rather than
/// at the first message whose shape has changed.
pub const PROTOCOL_VERSION: u32 = 2;

/// How many messages may be queued before a producer is made to wait, and how
/// much progress may pile up before it is dropped instead.
const QUEUE_DEPTH: usize = 256;

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
        };
        (connection, Events { incoming, queued })
    }

    /// The opening exchange. Establishes that both sides speak the same
    /// version before anything depends on a message shape.
    pub fn initialize(&self, timeout: Duration) -> Result<Value> {
        let reply = self.request(
            "initialize",
            json!({ "protocol_version": PROTOCOL_VERSION }),
            timeout,
        )?;
        match reply.get("protocol_version").and_then(Value::as_u64) {
            Some(theirs) if theirs as u32 == PROTOCOL_VERSION => Ok(reply),
            Some(theirs) => Err(EngineError::Rejected(format!(
                "sidecar speaks protocol {theirs}, this build speaks {PROTOCOL_VERSION}"
            ))),
            None => Err(EngineError::Transport(
                "sidecar did not state a protocol version".into(),
            )),
        }
    }

    /// Ask, and wait for the reply with this request's id — not for the next
    /// line to arrive, which may belong to someone else or to no one.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        if self.ended.load(Ordering::SeqCst) {
            return Err(EngineError::NotRunning);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (answer, wait) = sync_channel(1);
        self.pending.lock().expect("pending").insert(id, answer);

        let line = json!({ "id": id, "method": method, "params": params }).to_string();
        if self.outgoing.send(line).is_err() {
            self.pending.lock().expect("pending").remove(&id);
            return Err(EngineError::NotRunning);
        }

        match wait.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(EngineError::Rejected(error.to_string())),
            // The reader drops every waiting sender when the child ends, which
            // arrives here as a closed channel rather than as a timeout.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(EngineError::NotRunning),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.pending.lock().expect("pending").remove(&id);
                Err(EngineError::Transport(format!(
                    "{method} did not answer within {timeout:?}"
                )))
            }
        }
    }

    /// Progress events discarded because nothing was reading them.
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::SeqCst)
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
) {
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(message) = serde_json::from_str::<Value>(line) else {
                // Not a frame. Reported rather than ignored: on this channel it
                // means something is writing where the protocol lives.
                eprintln!("sidecar: unparseable protocol line: {line}");
                continue;
            };
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
