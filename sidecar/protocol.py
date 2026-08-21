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
import queue
import sys
import threading
import traceback
from dataclasses import dataclass, field
from typing import Any, Callable, Protocol

JSONRPC_VERSION = "2.0"

# The engine API's own version, which is about the methods rather than the wire.
API_VERSION = 1

# Requests waiting for the model actor. Bounded so a caller that submits faster
# than the engine works is refused rather than allowed to grow the queue without
# limit; the refusal is a reply, so it is visible.
ACTOR_QUEUE_DEPTH = 64

# How much may be queued for stdout before progress starts being dropped. The
# queue itself is unbounded, which is deliberate: blocking a sender would block
# either the actor mid-job or the reader mid-request, and a reader that stops
# reading cannot receive the drain that would release it.
OUTBOX_SOFT_LIMIT = 256

# The longest line either side will accept. A peer that sends more than this is
# not one of ours, and reading it would mean allocating whatever it claims.
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
    """The only thing that writes to stdout.

    Nothing that hands it a message ever waits. Progress is dropped once the
    backlog passes `OUTBOX_SOFT_LIMIT` — it is the same fact at successive
    values, and the newest supersedes what is queued. Everything else is kept,
    because a lost reply strands a caller and a lost terminal event strands a
    job. Blocking instead would stall the actor mid-operation, or the reader
    mid-request, and a reader that has stopped reading cannot receive whatever
    would have released it.
    """

    def __init__(self, stream) -> None:
        self._stream = stream
        self._outbox: queue.Queue = queue.Queue()
        self.dropped = 0
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def send(self, message: dict) -> None:
        coalescible = message.get("method") == "job.progress"
        if coalescible and self._outbox.qsize() >= OUTBOX_SOFT_LIMIT:
            self.dropped += 1
            return
        self._outbox.put(message)

    def _run(self) -> None:
        while True:
            message = self._outbox.get()
            if message is None:
                return
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

    def __init__(self, handlers: dict[str, Callable], writer: Writer, cancellation: Cancellation) -> None:
        self._handlers = handlers
        self._writer = writer
        self._cancellation = cancellation
        self._work: queue.Queue = queue.Queue(maxsize=ACTOR_QUEUE_DEPTH)
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def submit(
        self, request_id: Any, method: str, params: dict, is_notification: bool = False
    ) -> bool:
        """Queue an operation. False when the queue is full, so the caller can
        be refused rather than left waiting on a reply that will not come."""
        try:
            self._work.put_nowait((request_id, method, params, is_notification))
            return True
        except queue.Full:
            return False

    def depth(self) -> int:
        return self._work.qsize()

    def _run(self) -> None:
        while True:
            request_id, method, params, is_notification = self._work.get()
            fields = params if isinstance(params, dict) else {}
            job_id = fields.get("job_id")
            execution_id = fields.get("execution_id")
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
                if execution_id:
                    self._cancellation.forget(execution_id)


def _log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def serve(
    *,
    broker: dict[str, Callable],
    model: dict[str, Callable],
    capabilities: dict | None = None,
    stdin=None,
    stdout=None,
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
    writer = Writer(protocol_out)
    cancellation = Cancellation()
    actor = ModelActor(model, writer, cancellation)

    for line in source:
        line = line.strip()
        if not line:
            continue
        if len(line) > MAX_FRAME_BYTES:
            # Refused by size before it is parsed. A guard rather than a hard
            # bound: the line has already been read to find its end, so this
            # stops us acting on it, not receiving it.
            writer.error(None, INVALID_REQUEST, "frame exceeds the size limit")
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
                        "jsonrpc": JSONRPC_VERSION,
                        "api_version": API_VERSION,
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
                cancellation.request(execution_id)
                if not is_notification:
                    writer.reply(request_id, {"state": "cancel_requested"})
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
