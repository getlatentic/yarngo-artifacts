"""Serving the Yarngo engine API: JSON-RPC 2.0, one object per line, over stdio.

The wire is not ours: request ids that correlate replies independently of
order, notifications that expect no reply, and a defined error shape are all
JSON-RPC 2.0, and reinventing them would only mean getting them slightly wrong.
The methods are ours, and so is everything below — which is the part JSON-RPC
says nothing about.

The previous version read one request, ran it to completion, and wrote one
reply. While it worked it was not reading, so nothing could be asked and
nothing could be said — which is why progress and cancellation ended up
travelling by file, and why a cheap call waited behind a long one.

Three parts, each owning one thing:

    reader      the main thread. Parses, and decides where a request goes.
                It never touches the model, because it must never be slow.
    actor       one worker thread. Owns the model and everything derived from
                it, so nothing else can read a cache while it is being written.
    writer      one thread. The only thing that writes to stdout, so two
                answers cannot interleave halves of a line.

Requests are sorted by what they own, not by how long they take. A method that
looks cheap but reads model state still belongs to the actor: `list_voices`
reading the voice table while `register_voice` writes it is a torn read that no
timing test would catch.

What can overtake what: a broker reply can arrive before a model reply that was
asked for first, which is the whole point. Model operations cannot overtake each
other — the actor is one thread and runs them in the order they were queued.
"""

from __future__ import annotations

import json
import os
import queue
import sys
import threading
import time
import traceback
from collections import deque
from dataclasses import dataclass, field
from typing import Any, Callable, Protocol

JSONRPC_VERSION = "2.0"

# What this speaks, named so a peer can tell it apart from anything else that
# also happens to be JSON-RPC over a pipe. The wire is a standard; the methods
# are ours, and this is the version of those. It starts at one because nothing
# has shipped: there is no earlier version to be compatible with.
PROTOCOL_NAME = "yarngo-engine"
PROTOCOL_VERSION = 1

# Requests waiting for the model actor: one running, one ready to start. Shallow
# on purpose. A deep queue here would make this a second scheduler holding a
# backlog the application cannot see, reorder, or recover — and the application
# is the one that knows what a restart should retry. Deeper than one so the
# engine is not idle between operations; no deeper, because nothing here can be
# rescheduled.
ACTOR_QUEUE_DEPTH = 2

# How much may be waiting on the control lane. Bounded, and its ceiling is
# calculable rather than hoped for: a reply exists only because a request was
# read, admission is bounded by the actor's queue, and terminal events are one
# per execution. Reaching this means an assumption is wrong, so it is fatal
# rather than dropped — a lost reply strands a caller for ever.
CONTROL_LANE_LIMIT = 512

# How many finished executions to remember, so a cancellation arriving after the
# work ended can say so rather than calling it unknown. Bounded and not
# persisted: this is a courtesy for late messages, never a record. The
# application keeps the history, and after a restart an old execution is
# genuinely unknown here — which is the truth, since whatever was running died
# with the process.
FINISHED_MEMORY = 256

# The longest line either side will accept, enforced while reading rather than
# after. A peer sending more than this is not one of ours, and finding the end
# of its line before objecting would mean allocating whatever it claimed.
MAX_FRAME_BYTES = 8 * 1024 * 1024

# JSON-RPC's own codes, for faults in the exchange itself.
PARSE_ERROR = -32700
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603

# Ours, in the range JSON-RPC leaves to the server. Numbers rather than message
# matching: the caller decides what to do from these, and rewording a message
# must not change behaviour.
ENGINE_BUSY = -32001
VOICE_DELETED = -32002
MODEL_NOT_INSTALLED = -32003
JOB_ALREADY_TERMINAL = -32004


# What a model operation is: parameters, and the context it reports against.
Handler = Callable[[dict, "Context"], "dict | None"]
# What a broker operation is: parameters alone. Nothing it answers runs long
# enough to have progress to report or a cancellation to honour.
BrokerHandler = Callable[[dict], "dict | None"]


class Cancelled(Exception):
    """Raised by a handler that stopped because it was asked to.

    Not a failure: the work did not finish, and that is the outcome the caller
    asked for. Reported as a cancellation with its own terminal event, because a
    job sitting in `cancel_requested` is waiting to be told how it ended and an
    error would tell it the wrong thing.
    """


class Emit(Protocol):
    def __call__(self, method: str, params: dict) -> None: ...


@dataclass
class Cancellation:
    """Which executions have been asked to stop.

    Keyed on the execution rather than the job. A job may be attempted more than
    once — after an engine restart, or a retry — and a cancellation aimed at the
    attempt that was abandoned must not reach the one that replaced it. Rust
    owns which attempt is current; this only has to answer about the one it was
    told to run.

    Written by the reader thread the moment a cancellation arrives, read by the
    actor at whatever checkpoint it reaches next. That gap is the honest cost of
    cooperative cancellation: the request is acknowledged immediately, and the
    work stops when it can.
    """

    _asked: set[str] = field(default_factory=set)
    _lock: threading.Lock = field(default_factory=threading.Lock)

    def request(self, execution_id: str) -> None:
        with self._lock:
            self._asked.add(execution_id)

    def is_requested(self, execution_id: str) -> bool:
        with self._lock:
            return execution_id in self._asked

    def forget(self, execution_id: str) -> None:
        with self._lock:
            self._asked.discard(execution_id)


@dataclass
class Context:
    """What a model operation is given beyond its parameters.

    Both identifiers travel with everything it says. The job is what the user
    started and what Rust keeps; the execution is this attempt at it. An event
    naming only the job could be mistaken for the current attempt when it came
    from an abandoned one.
    """

    emit_raw: Emit
    cancellation: Cancellation
    job_id: str | None = None
    execution_id: str | None = None

    def emit(self, method: str, params: dict) -> None:
        stamped = dict(params)
        if self.job_id is not None:
            stamped.setdefault("job_id", self.job_id)
        if self.execution_id is not None:
            stamped.setdefault("execution_id", self.execution_id)
        self.emit_raw(method, stamped)

    def cancelled(self) -> bool:
        return self.execution_id is not None and self.cancellation.is_requested(
            self.execution_id
        )


class Writer:
    """The only thing that writes to stdout, in two lanes.

    A bounded queue, never blocking, and never losing anything cannot all hold
    at once, so this says which gives. Progress is coalesced: one entry per
    execution, replaced as newer values arrive, because it is the same fact at
    successive values and only the newest is worth sending. Its memory is
    therefore the number of live executions, not the number of updates.

    The control lane — replies, errors, terminal events — loses nothing. It is
    bounded, but its ceiling is calculable rather than hoped for: a reply exists
    only because a request was read, admission is bounded by the actor's queue,
    and a terminal event is one per execution. Reaching the limit means one of
    those is untrue, and a dropped reply would strand a caller silently, so it
    ends the session instead and lets the parent start a fresh one.

    Nothing that hands it a message waits. Blocking would stall the actor
    mid-operation or the reader mid-request, and a reader that has stopped
    reading cannot receive whatever would have released it.
    """

    def __init__(self, stream, on_fatal=None) -> None:
        self._stream = stream
        self._control: deque = deque()
        self._progress: dict[str, dict] = {}
        self._ready = threading.Condition()
        self._on_fatal = on_fatal or _die
        self._closing = False
        self.coalesced = 0
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def send(self, message: dict) -> None:
        key = _progress_key(message)
        with self._ready:
            if key is not None:
                if key in self._progress:
                    self.coalesced += 1
                self._progress[key] = message
            else:
                if len(self._control) >= CONTROL_LANE_LIMIT:
                    self._on_fatal(
                        f"control lane exceeded {CONTROL_LANE_LIMIT} messages; "
                        "a reply would have to be dropped"
                    )
                    return
                self._control.append(message)
            self._ready.notify()

    def close(self, timeout: float = 5.0) -> None:
        """Wait for what is queued to be written.

        The writer is a daemon thread, so without this the process can end
        between a reply being handed over and it reaching the pipe — and the
        caller waiting on that reply learns nothing except that the engine
        stopped.
        """
        with self._ready:
            self._closing = True
            self._ready.notify()
        self._thread.join(timeout)

    def _run(self) -> None:
        while True:
            with self._ready:
                while not self._control and not self._progress and not self._closing:
                    self._ready.wait()
                if self._closing and not self._control and not self._progress:
                    return
                # Control first and in full: progress that is superseded while
                # a reply is being written was never worth sending.
                batch = list(self._control)
                self._control.clear()
                batch.extend(self._progress.values())
                self._progress.clear()
            for message in batch:
                self._stream.write(json.dumps(message) + "\n")
                self._stream.flush()

    def reply(self, request_id: Any, result: dict) -> None:
        self.send({"jsonrpc": JSONRPC_VERSION, "id": request_id, "result": result})

    def error(self, request_id: Any, code: int, message: str, data: dict | None = None) -> None:
        payload = {"code": code, "message": message}
        if data is not None:
            payload["data"] = data
        self.send({"jsonrpc": JSONRPC_VERSION, "id": request_id, "error": payload})

    def event(self, method: str, params: dict) -> None:
        """A notification: no id, and no reply is expected or permitted."""
        self.send({"jsonrpc": JSONRPC_VERSION, "method": method, "params": params})


class ModelActor:
    """One thread, and the only one that may touch the model.

    Everything derived from a model — the loaded weights, the conditioning
    cache, the generator — is reachable from exactly here. Operations queue
    rather than run concurrently, which is a deliberate limit: a desktop
    accelerator gains nothing from two generations at once, and the caches
    behind them are not written to be shared.
    """

    def __init__(self, handlers: dict[str, Handler], writer: Writer, cancellation: Cancellation) -> None:
        self._handlers = handlers
        self._writer = writer
        self._cancellation = cancellation
        self._work: queue.Queue = queue.Queue(maxsize=ACTOR_QUEUE_DEPTH)
        # What this engine was told to run, so a cancellation can be answered
        # with what happened rather than with an acknowledgement either way.
        # Knowing its own executions is not scheduling: the caller decides what
        # to run and in what order.
        self._lock = threading.Lock()
        self._queued: set[str] = set()
        self._running: str | None = None
        # Whether a handler is in progress, which is not the same question as
        # which execution is in progress: most operations have no execution at
        # all, and `_running is None` is their normal state while working.
        self._busy = False
        self._finished: deque = deque(maxlen=FINISHED_MEMORY)
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def state_of(self, execution_id: str) -> str:
        with self._lock:
            if execution_id == self._running:
                return "running"
            if execution_id in self._queued:
                return "queued"
            if execution_id in self._finished:
                return "terminal"
        return "unknown"

    def submit(
        self, request_id: Any, method: str, params: dict, is_notification: bool = False
    ) -> bool:
        """Queue an operation. False when the queue is full, so the caller can
        be refused rather than left waiting on a reply that will not come."""
        execution_id = params.get("execution_id") if isinstance(params, dict) else None
        try:
            self._work.put_nowait((request_id, method, params, is_notification))
        except queue.Full:
            return False
        if execution_id:
            with self._lock:
                self._queued.add(execution_id)
        return True

    def depth(self) -> int:
        return self._work.qsize()

    def drain(self, timeout: float = 30.0) -> None:
        """Wait for what has been accepted to finish.

        Input ending does not end the work already taken from it. Closing the
        writer first would lose the answer to a request that was accepted, which
        is the one kind of silence a caller cannot recover from.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self._lock:
                idle = not self._busy
            if idle and self._work.empty():
                return
            time.sleep(0.01)

    def _run(self) -> None:
        while True:
            request_id, method, params, is_notification = self._work.get()
            fields = params if isinstance(params, dict) else {}
            job_id = fields.get("job_id")
            execution_id = fields.get("execution_id")
            with self._lock:
                self._queued.discard(execution_id)
                self._running = execution_id
                self._busy = True
            # Checked before the handler is entered, not only inside it: an
            # execution cancelled while it waited its turn should never start.
            if execution_id and self._cancellation.is_requested(execution_id):
                self._cancellation.forget(execution_id)
                with self._lock:
                    self._running = None
                    self._busy = False
                    self._finished.append(execution_id)
                # The terminal event, not only the reply. The caller's job is
                # sitting in `cancel_requested` waiting to be told how it ended,
                # and a reply to the submission does not say that: the two are
                # answers to different questions.
                self._writer.event(
                    "job.cancelled",
                    {"job_id": job_id, "execution_id": execution_id, "started": False},
                )
                if not is_notification:
                    self._writer.reply(request_id, {"state": "cancelled", "started": False})
                continue
            context = Context(
                emit_raw=self._writer.event,
                cancellation=self._cancellation,
                job_id=job_id,
                execution_id=execution_id,
            )
            try:
                result = self._handlers[method](params, context)
                if not is_notification:
                    self._writer.reply(request_id, result if result is not None else {})
            except Cancelled as stopped:
                # Started and then stopped, which is a different fact from
                # stopped before starting: work was done and any partial output
                # exists. Both say `started` so the caller can tell them apart.
                self._writer.event(
                    "job.cancelled",
                    {
                        "job_id": job_id,
                        "execution_id": execution_id,
                        "started": True,
                        "detail": str(stopped),
                    },
                )
                if not is_notification:
                    self._writer.reply(request_id, {"state": "cancelled", "started": True})
            except Exception as exc:  # noqa: BLE001
                # Reported as a reply, never as a stack trace on stdout: the
                # caller is waiting on this id and gets an error rather than a
                # timeout, and the trace goes to the log.
                _log(traceback.format_exc())
                if not is_notification:
                    self._writer.error(
                        request_id, INTERNAL_ERROR, f"{type(exc).__name__}: {exc}"
                    )
            finally:
                with self._lock:
                    self._running = None
                    self._busy = False
                    if execution_id:
                        self._finished.append(execution_id)
                if execution_id:
                    self._cancellation.forget(execution_id)


def _log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def _die(reason: str) -> None:
    """End the session. The parent notices the output stop and starts another."""
    _log(f"protocol failure: {reason}")
    sys.stderr.flush()
    os._exit(70)


def _progress_key(message: dict) -> str | None:
    """Which stream of progress this supersedes, or None if it supersedes
    nothing. Keyed per execution so one job's updates cannot displace another's."""
    if message.get("method") != "job.progress":
        return None
    params = message.get("params") or {}
    return str(params.get("execution_id") or params.get("job_id") or "")


def serve(
    *,
    broker: dict[str, BrokerHandler],
    model: dict[str, Handler],
    capabilities: dict | None = None,
    stdin=None,
    stdout=None,
    on_fatal=None,
) -> None:
    """Read requests until the input ends.

    `broker` methods run on this thread and must not touch model state.
    `model` methods run on the actor, one at a time.
    """
    # The protocol keeps the real stdout, and everything else is pointed at the
    # log. A library writing a progress bar to stdout would otherwise put a line
    # on the wire that is not a frame, and one such line is enough.
    protocol_out = stdout if stdout is not None else sys.stdout
    if stdout is None:
        sys.stdout = sys.stderr

    source = stdin if stdin is not None else sys.stdin
    on_fatal = on_fatal or _die
    writer = Writer(protocol_out, on_fatal=on_fatal)
    cancellation = Cancellation()
    actor = ModelActor(model, writer, cancellation)

    while True:
        # Read at most one frame's worth. A line longer than the limit comes
        # back without its newline, which is how an oversized frame is told
        # apart from a large-but-legal one.
        raw = source.readline(MAX_FRAME_BYTES + 1)
        if raw == "":
            break
        if len(raw) > MAX_FRAME_BYTES and not raw.endswith("\n"):
            # No attempt to resynchronise: the rest of that frame is still in
            # the stream, and guessing where it ends is how a parser starts
            # reading someone else's bytes as a message.
            writer.error(None, INVALID_REQUEST, "frame exceeds the size limit")
            on_fatal("oversized frame")
            return
        line = raw.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            # A parse failure has no id to answer against, and null is what the
            # specification asks for in exactly this case.
            writer.error(None, PARSE_ERROR, f"bad json: {exc}")
            continue

        problem = _envelope_fault(request)
        if problem is not None:
            writer.error(
                request.get("id") if isinstance(request, dict) else None,
                INVALID_REQUEST,
                problem,
            )
            continue

        method = request["method"]
        # Kept as sent. `or {}` turned an empty positional list into an object,
        # which is a different call.
        params = request["params"] if "params" in request else {}
        if params is None:
            params = {}
        # Absent rather than null: a request without an id is a notification,
        # and answering one is as wrong as failing to answer a request.
        request_id = request.get("id")
        is_notification = "id" not in request

        if method == "initialize":
            if not is_notification:
                writer.reply(
                    request_id,
                    {
                        "protocol": PROTOCOL_NAME,
                        "version": PROTOCOL_VERSION,
                        **(capabilities or {}),
                    },
                )
            continue

        if method == "job.cancel":
            # Answered here rather than queued, which is the point of it: a
            # cancellation that waited its turn behind the job it is cancelling
            # would arrive after the work it was meant to stop.
            # By execution rather than by job: the caller knows which attempt is
            # current, and a cancellation meant for an abandoned one must not
            # reach its replacement.
            execution_id = params.get("execution_id") if isinstance(params, dict) else None
            if not execution_id:
                if not is_notification:
                    writer.error(
                        request_id, INVALID_PARAMS, "job.cancel needs an execution_id"
                    )
            else:
                # Answered with what is true of this execution. A blanket
                # acknowledgement would tell a caller its cancellation landed
                # when the work had already finished, or never existed.
                state = actor.state_of(execution_id)
                if state in ("queued", "running"):
                    cancellation.request(execution_id)
                outcome = {
                    "queued": "cancel_requested",
                    "running": "cancel_requested",
                    "terminal": "already_terminal",
                    "unknown": "unknown_execution",
                }[state]
                if not is_notification:
                    writer.reply(request_id, {"state": outcome, "execution_state": state})
            continue

        if method in broker:
            try:
                result = broker[method](params) or {}
                if not is_notification:
                    writer.reply(request_id, result)
            except Exception as exc:  # noqa: BLE001
                _log(traceback.format_exc())
                if not is_notification:
                    writer.error(request_id, INTERNAL_ERROR, f"{type(exc).__name__}: {exc}")
            continue

        if method in model:
            if not actor.submit(request_id, method, params, is_notification):
                if not is_notification:
                    writer.error(
                        request_id,
                        ENGINE_BUSY,
                        "the engine has too much queued",
                        {"queue_depth": actor.depth()},
                    )
            continue

        if not is_notification:
            writer.error(request_id, METHOD_NOT_FOUND, f"unknown method {method!r}")

    # The input has ended, so nothing more will be asked. What was already
    # accepted still has to finish, and what it answers still has to arrive.
    actor.drain()
    writer.close()


def _envelope_fault(request: Any) -> str | None:
    """Why this is not a JSON-RPC request, or None when it is one."""
    if isinstance(request, list):
        # A valid JSON-RPC construction this profile does not serve. Refused by
        # name, so a caller learns which it is rather than guessing from a
        # generic complaint about shape.
        return "batches are not supported by this engine"
    if not isinstance(request, dict):
        return "a request must be an object"
    if request.get("jsonrpc") != JSONRPC_VERSION:
        return f"expected jsonrpc {JSONRPC_VERSION!r}, got {request.get('jsonrpc')!r}"
    method = request.get("method")
    if not isinstance(method, str):
        return "method must be a string"
    params = request.get("params")
    if params is not None and not isinstance(params, (dict, list)):
        return "params must be an object or an array"
    if "id" in request and not isinstance(request["id"], (str, int, float, type(None))):
        return "id must be a string, a number, or null"
    return None
