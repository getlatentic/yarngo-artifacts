"""Serving the version 2 wire: keep reading, answer out of order, speak freely.

Version 1 read one request, ran it to completion, and wrote one reply. While it
worked it was not reading, so nothing could be asked and nothing could be said —
which is why progress and cancellation ended up travelling by file, and why a
cheap call waited behind a long one.

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
"""

from __future__ import annotations

import json
import queue
import sys
import threading
import traceback
from dataclasses import dataclass, field
from typing import Any, Callable, Protocol

PROTOCOL_VERSION = 2

# Requests waiting for the model actor. Bounded so a caller that submits faster
# than the engine works is refused rather than allowed to grow the queue without
# limit; the refusal is a reply, so it is visible.
ACTOR_QUEUE_DEPTH = 64

# Error codes carried on the wire. Numbers rather than message matching: the
# caller decides what to do from these, and a reworded message must not change
# behaviour.
ERROR_UNKNOWN_METHOD = 404
ERROR_BAD_REQUEST = 400
ERROR_BUSY = 429
ERROR_FAILED = 500


class Emit(Protocol):
    def __call__(self, method: str, params: dict) -> None: ...


@dataclass
class Cancellation:
    """Which jobs have been asked to stop.

    Written by the reader thread the moment a cancellation arrives, read by the
    actor at whatever checkpoint it reaches next. That gap is the honest cost of
    cooperative cancellation: the request is acknowledged immediately, and the
    work stops when it can.
    """

    _asked: set[str] = field(default_factory=set)
    _lock: threading.Lock = field(default_factory=threading.Lock)

    def request(self, job_id: str) -> None:
        with self._lock:
            self._asked.add(job_id)

    def is_requested(self, job_id: str) -> bool:
        with self._lock:
            return job_id in self._asked

    def forget(self, job_id: str) -> None:
        with self._lock:
            self._asked.discard(job_id)


@dataclass
class Context:
    """What a model operation is given beyond its parameters."""

    emit: Emit
    cancellation: Cancellation
    job_id: str | None = None

    def cancelled(self) -> bool:
        return self.job_id is not None and self.cancellation.is_requested(self.job_id)


class Writer:
    """The only thing that writes to stdout."""

    def __init__(self, stream) -> None:
        self._stream = stream
        self._outbox: queue.Queue = queue.Queue()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def send(self, message: dict) -> None:
        self._outbox.put(message)

    def _run(self) -> None:
        while True:
            message = self._outbox.get()
            if message is None:
                return
            self._stream.write(json.dumps(message) + "\n")
            self._stream.flush()

    def reply(self, request_id: Any, result: dict) -> None:
        self.send({"id": request_id, "result": result})

    def error(self, request_id: Any, code: int, message: str, data: dict | None = None) -> None:
        payload = {"code": code, "message": message}
        if data is not None:
            payload["data"] = data
        self.send({"id": request_id, "error": payload})

    def event(self, method: str, params: dict) -> None:
        self.send({"method": method, "params": params})


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

    def submit(self, request_id: Any, method: str, params: dict) -> bool:
        """Queue an operation. False when the queue is full, so the caller can
        be refused rather than left waiting on a reply that will not come."""
        try:
            self._work.put_nowait((request_id, method, params))
            return True
        except queue.Full:
            return False

    def depth(self) -> int:
        return self._work.qsize()

    def _run(self) -> None:
        while True:
            request_id, method, params = self._work.get()
            job_id = params.get("job_id") if isinstance(params, dict) else None
            context = Context(
                emit=self._writer.event,
                cancellation=self._cancellation,
                job_id=job_id,
            )
            try:
                result = self._handlers[method](params, context)
                self._writer.reply(request_id, result if result is not None else {})
            except Exception as exc:  # noqa: BLE001
                # Reported as a reply, never as a stack trace on stdout: the
                # caller is waiting on this id and gets an error rather than a
                # timeout, and the trace goes to the log.
                _log(traceback.format_exc())
                self._writer.error(
                    request_id, ERROR_FAILED, f"{type(exc).__name__}: {exc}"
                )
            finally:
                if job_id:
                    self._cancellation.forget(job_id)


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
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            writer.error(None, ERROR_BAD_REQUEST, f"bad json: {exc}")
            continue

        request_id = request.get("id")
        method = request.get("method")
        params = request.get("params") or {}

        if method == "initialize":
            writer.reply(
                request_id,
                {"protocol_version": PROTOCOL_VERSION, **(capabilities or {})},
            )
            continue

        if method == "job.cancel":
            # Answered here rather than queued, which is the point of it: a
            # cancellation that waited its turn behind the job it is cancelling
            # would arrive after the work it was meant to stop.
            job_id = params.get("job_id")
            if not job_id:
                writer.error(request_id, ERROR_BAD_REQUEST, "job.cancel needs a job_id")
            else:
                cancellation.request(job_id)
                writer.reply(request_id, {"state": "cancel_requested"})
            continue

        if method in broker:
            try:
                writer.reply(request_id, broker[method](params) or {})
            except Exception as exc:  # noqa: BLE001
                _log(traceback.format_exc())
                writer.error(request_id, ERROR_FAILED, f"{type(exc).__name__}: {exc}")
            continue

        if method in model:
            if not actor.submit(request_id, method, params):
                writer.error(
                    request_id,
                    ERROR_BUSY,
                    "the engine has too much queued",
                    {"queue_depth": actor.depth()},
                )
            continue

        writer.error(request_id, ERROR_UNKNOWN_METHOD, f"unknown method {method!r}")
