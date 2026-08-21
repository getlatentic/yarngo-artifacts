"""Whether the serving layer is actually JSON-RPC 2.0, and not merely shaped like it.

The Rust client can only send well-formed frames, so it cannot ask what happens
to a malformed one. These drive `serve()` directly with the faults a real peer
can produce: no version, the wrong version, a method that is not a string,
something that is not an object at all, and a notification — which must be acted
on and not answered.

    python3 sidecar/test_protocol.py
"""

from __future__ import annotations

import io
import json
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import protocol  # noqa: E402


class Collected:
    """Somewhere for the writer to put lines, readable from the test."""

    def __init__(self) -> None:
        self.lines: list[str] = []
        self._lock = threading.Lock()

    def write(self, text: str) -> int:
        if text.strip():
            with self._lock:
                self.lines.append(text.strip())
        return len(text)

    def flush(self) -> None:
        pass

    def messages(self) -> list[dict]:
        with self._lock:
            return [json.loads(line) for line in self.lines]


def exchange(
    requests: list[str],
    *,
    expect: int,
    broker=None,
    model=None,
    fatals: list | None = None,
    settle: float = 2.0,
    until=None,
) -> list[dict]:
    """Feed raw lines in, and collect what comes out."""
    out = Collected()
    record = fatals if fatals is not None else []
    served = threading.Thread(
        target=protocol.serve,
        kwargs={
            "broker": broker or {"ping": lambda params: {"pong": True}},
            "model": model or {},
            "capabilities": {},
            "stdin": io.StringIO("\n".join(requests) + "\n"),
            "stdout": out,
            "on_fatal": record.append,
        },
        daemon=True,
    )
    served.start()
    served.join(timeout=5)
    # `serve` returns when its input ends, which can be long before the actor
    # has finished and the writer has drained. Wait for what is expected rather
    # than for the reader to stop.
    deadline = time.monotonic() + settle
    # A line count is the wrong thing to wait for once progress collapses: three
    # progress lines satisfy `expect=3` long before the reply is written.
    done = until or (lambda messages: len(messages) >= expect)
    while time.monotonic() < deadline and not done(out.messages()):
        time.sleep(0.01)
    return out.messages()


def frame(**fields) -> str:
    return json.dumps({"jsonrpc": "2.0", **fields})


def check(name: str, condition: bool, failures: list) -> None:
    if not condition:
        failures.append(name)


def main() -> int:
    failures: list[str] = []

    # Every reply carries the version.
    replies = exchange([frame(id=1, method="ping")], expect=1)
    check("a reply is JSON-RPC 2.0", replies[0].get("jsonrpc") == "2.0", failures)
    check("a reply carries its id", replies[0].get("id") == 1, failures)

    # An id may be a string as well as a number, and comes back unchanged.
    replies = exchange([frame(id="abc", method="ping")], expect=1)
    check("a string id is echoed", replies[0].get("id") == "abc", failures)

    # A notification is acted on and never answered.
    seen: list[dict] = []
    replies = exchange(
        [frame(method="ping"), frame(id=9, method="ping")],
        expect=1,
        broker={"ping": lambda params: seen.append(params) or {"pong": True}},
    )
    check("a notification is acted on", len(seen) == 2, failures)
    check("a notification is not answered", len(replies) == 1, failures)
    check("only the request was answered", replies[0].get("id") == 9, failures)

    # Faults in the exchange, reported with the codes the specification names.
    cases = [
        ("not json at all", "{oh no", protocol.PARSE_ERROR),
        ("a message with no version", json.dumps({"id": 1, "method": "ping"}), protocol.INVALID_REQUEST),
        ("the wrong version", json.dumps({"jsonrpc": "1.0", "id": 1, "method": "ping"}), protocol.INVALID_REQUEST),
        ("a method that is not a string", frame(id=1, method=123), protocol.INVALID_REQUEST),
        ("something that is not an object", json.dumps([1, 2, 3]), protocol.INVALID_REQUEST),
        ("params that are not a structure", json.dumps({"jsonrpc": "2.0", "id": 1, "method": "ping", "params": 7}), protocol.INVALID_REQUEST),
        ("a method nobody serves", frame(id=1, method="no.such.thing"), protocol.METHOD_NOT_FOUND),
    ]
    for name, line, expected in cases:
        replies = exchange([line], expect=1)
        got = replies[0].get("error", {}).get("code") if replies else None
        check(f"{name} is {expected}", got == expected, failures)

    # A parse failure has no id to answer against, and null is what to send.
    replies = exchange(["{oh no"], expect=1)
    check("a parse error answers against null", replies[0].get("id") is None, failures)

    # job.cancel without a job is bad parameters, not a bad request.
    replies = exchange([frame(id=1, method="job.cancel", params={})], expect=1)
    check(
        "job.cancel with no job_id is invalid params",
        replies[0].get("error", {}).get("code") == protocol.INVALID_PARAMS,
        failures,
    )

    # A handler that raises answers with an internal error, not a stack trace.
    def explodes(params, ctx):
        raise ValueError("nope")

    replies = exchange([frame(id=1, method="boom")], expect=1, model={"boom": explodes})
    check(
        "a raising handler is an internal error",
        replies[0].get("error", {}).get("code") == protocol.INTERNAL_ERROR,
        failures,
    )
    check(
        "the error says what went wrong",
        "nope" in replies[0].get("error", {}).get("message", ""),
        failures,
    )

    # An event is a notification: a method, no id.
    def emits(params, ctx):
        ctx.emit("job.progress", {"job_id": "j1", "completed": 1})
        return {}

    messages = exchange([frame(id=1, method="work")], expect=2, model={"work": emits})
    events = [m for m in messages if "method" in m]
    check("an event is sent", len(events) == 1, failures)
    check("an event carries no id", "id" not in events[0], failures)
    check("an event is JSON-RPC 2.0", events[0].get("jsonrpc") == "2.0", failures)
    check("an event names its method", events[0].get("method") == "job.progress", failures)

    # Positional params are a different call from an empty object, and must
    # arrive as sent.
    seen: list = []
    exchange(
        [frame(id=1, method="ping", params=[])],
        expect=1,
        broker={"ping": lambda params: seen.append(params) or {}},
    )
    check("an empty positional list is preserved", seen == [[]], failures)

    seen.clear()
    exchange(
        [frame(id=1, method="ping", params=["a", 2])],
        expect=1,
        broker={"ping": lambda params: seen.append(params) or {}},
    )
    check("positional params arrive as a list", seen == [["a", 2]], failures)

    # A batch is valid JSON-RPC that this profile does not serve, and says so.
    replies = exchange([json.dumps([{"jsonrpc": "2.0", "id": 1, "method": "ping"}])], expect=1)
    check(
        "a batch is refused by name",
        "batch" in replies[0].get("error", {}).get("message", "").lower(),
        failures,
    )

    # An explicit null id is a request, not a notification, and is answered.
    replies = exchange([json.dumps({"jsonrpc": "2.0", "id": None, "method": "ping"})], expect=1)
    check("an explicit null id is answered", len(replies) == 1, failures)
    check("and answered against null", replies[0].get("id") is None, failures)

    # Oversized frames are refused before they are parsed.
    huge = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "ping", "params": {"x": "y" * (9 * 1024 * 1024)}})
    fatals: list = []
    replies = exchange([huge, frame(id=2, method="ping")], expect=1, fatals=fatals)
    check(
        "an oversized frame is refused",
        replies and replies[0].get("error", {}).get("message") == "frame exceeds the size limit",
        failures,
    )
    check("an oversized frame ends the session", len(fatals) == 1, failures)
    check(
        "nothing after an oversized frame is served",
        all(m.get("id") != 2 for m in replies),
        failures,
    )

    # Cancellation is keyed on the execution, so a stale one cannot reach the
    # attempt that replaced it.
    ran: list = []

    def watches(params, ctx):
        for _ in range(20):
            if ctx.cancelled():
                ran.append(("stopped", ctx.execution_id))
                return {}
            time.sleep(0.02)
        ran.append(("finished", ctx.execution_id))
        return {}

    exchange(
        [
            # Carries the job the next attempt also has: keyed on the job,
            # this cancellation would reach it.
            frame(id=1, method="job.cancel", params={"job_id": "abc", "execution_id": "abc/1"}),
            frame(id=2, method="watch", params={"job_id": "abc", "execution_id": "abc/2"}),
        ],
        expect=2,
        model={"watch": watches},
    )
    check(
        "a cancellation for one attempt does not stop another",
        ran and ran[-1][0] == "finished",
        failures,
    )

    ran.clear()
    exchange(
        [
            frame(id=1, method="job.cancel", params={"job_id": "abc", "execution_id": "abc/2"}),
            frame(id=2, method="watch", params={"job_id": "abc", "execution_id": "abc/2"}),
        ],
        expect=2,
        model={"watch": watches},
    )
    check(
        "a cancellation for this attempt does stop it",
        ran and ran[-1][0] == "stopped",
        failures,
    )

    # Events carry both identifiers, so one from an abandoned attempt cannot be
    # mistaken for the current one.
    def emits_both(params, ctx):
        ctx.emit("job.progress", {"completed": 1})
        return {}

    messages = exchange(
        [frame(id=1, method="work", params={"job_id": "abc", "execution_id": "abc/7"})],
        expect=2,
        model={"work": emits_both},
    )
    events = [m for m in messages if "method" in m]
    check("an event names its job", events[0]["params"].get("job_id") == "abc", failures)
    check("an event names its execution", events[0]["params"].get("execution_id") == "abc/7", failures)

    # Saturation: a flood of progress against exactly one reply and one
    # terminal event. Progress may collapse; neither of the others may.
    def floods(params, ctx):
        for i in range(100_000):
            ctx.emit("job.progress", {"completed": i})
        ctx.emit("job.completed", {})
        return {"done": True}

    messages = exchange(
        [frame(id=1, method="flood", params={"job_id": "j", "execution_id": "j/1"})],
        expect=3,
        model={"flood": floods},
        settle=30.0,
        until=lambda ms: any("id" in m for m in ms)
        and any(m.get("method") == "job.completed" for m in ms),
    )
    replies = [m for m in messages if "id" in m]
    completed = [m for m in messages if m.get("method") == "job.completed"]
    progress = [m for m in messages if m.get("method") == "job.progress"]
    check("the reply arrives exactly once", len(replies) == 1, failures)
    check("the terminal event arrives exactly once", len(completed) == 1, failures)
    check("progress collapsed rather than queued", len(progress) < 100_000, failures)
    check(
        "the progress that survived is the newest",
        progress and progress[-1]["params"]["completed"] == 99_999,
        failures,
    )

    # One job's progress cannot displace another's.
    def two_streams(params, ctx):
        for i in range(500):
            ctx.emit_raw("job.progress", {"execution_id": "a/1", "completed": i})
            ctx.emit_raw("job.progress", {"execution_id": "b/1", "completed": i})
        return {}

    messages = exchange(
        [frame(id=1, method="two")],
        expect=2,
        model={"two": two_streams},
        settle=10.0,
        until=lambda ms: any("id" in m for m in ms),
    )
    streams = {
        m["params"]["execution_id"]
        for m in messages
        if m.get("method") == "job.progress"
    }
    check("progress is coalesced per execution", streams == {"a/1", "b/1"}, failures)

    for failure in failures:
        print(f"FAIL: {failure}", file=sys.stderr)
    total = 40
    print(f"{total - len(failures)}/{total} protocol checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
