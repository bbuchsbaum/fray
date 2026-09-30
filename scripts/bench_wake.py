#!/usr/bin/env python3
"""Measure how fast published attention reaches a waiting host, per delivery path.

Epic child 3 (Mote bd-01M3RZ9BN4RYQRY56RFEPSTEJX), step 0: the baseline.
Each trial publishes one question through the real CLI and times its arrival
at a stand-in host on each path:

  wait    a blocking `fray wait --new` process returns
  watch   a long-running `fray watch --attention --notification` prints a line
          (what the Claude Monitor plugin runs)
  drive   `fray drive` starts its child with the packet (stub child, no model)
  hook    a `fray hook` PostToolUse call returns context naming the item

Latency is measured from the publisher's `send` returning to the host-visible
event, on one monotonic clock shared by all processes. It covers publication
and delivery to the host process. It does not measure host scheduling of an
idle model or model response time; those need real hosts (see the epic).
Failed and timed-out trials are counted and reported, never dropped.
"""
import argparse
import json
import os
import pathlib
import platform
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time

parser = argparse.ArgumentParser(
    description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
)
parser.add_argument("binary", nargs="?", default="target/release/fray")
parser.add_argument(
    "--n", type=int, default=30, help="trials per path (declared up front)"
)
parser.add_argument("--paths", default="wait,watch,drive,hook")
parser.add_argument(
    "--timeout",
    type=float,
    default=20.0,
    help="seconds before a trial counts as failed",
)
parser.add_argument(
    "--settle",
    type=float,
    default=0.3,
    help="seconds to let a waiter arm before publishing",
)
parser.add_argument(
    "--home-parent",
    default="/tmp",
    help="short local directory for the temporary board",
)
parser.add_argument(
    "--priority", type=int, default=2, choices=range(4),
    help="priority of published asks for wait/watch/drive (hook always p1)",
)
parser.add_argument("--json", action="store_true", help="print only the JSON result")
args = parser.parse_args()
binary = str(pathlib.Path(args.binary).resolve())
if not pathlib.Path(binary).is_file():
    parser.error("build first, e.g. cargo build --release")
if not 1 <= args.n <= 1000:
    parser.error("--n must be in 1..1000")
paths = [p for p in args.paths.split(",") if p]
unknown = set(paths) - {"wait", "watch", "drive", "hook"}
if unknown:
    parser.error(f"unknown paths: {sorted(unknown)}")

now = time.monotonic_ns


def distribution(xs):
    if not xs:
        return {"n": 0, "p50_ms": None, "p95_ms": None, "max_ms": None}
    xs = sorted(xs)

    def rank(q):
        return round(
            xs[min(len(xs) - 1, max(0, int(-(-q * len(xs) // 1)) - 1))] / 1e6, 2
        )

    return {
        "n": len(xs),
        "p50_ms": rank(0.5),
        "p95_ms": rank(0.95),
        "max_ms": round(xs[-1] / 1e6, 2),
    }


class Board:
    def __init__(self, home):
        self.home = home
        self.env = dict(os.environ)
        for key in (
            "FRAY_AGENT",
            "FRAY_SESSION",
            "FRAY_SELECTION",
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
        ):
            self.env.pop(key, None)
        self.log = open(os.path.join(home, "server.log"), "w")
        self.server = subprocess.Popen(
            [binary, "--home", home, "serve"], stdout=self.log, stderr=self.log
        )
        for _ in range(200):
            if self.run("pub", "ping", check=False).returncode == 0:
                break
            time.sleep(0.025)
        else:
            raise RuntimeError("daemon did not start")

    def argv(self, actor, *rest):
        return [binary, "--home", self.home, "--as", actor, *rest]

    def cenv(self, actor):
        env = dict(self.env)
        env["FRAY_SESSION"] = f"bench:{actor}"
        return env

    def run(self, actor, *rest, check=True, stdin=None):
        r = subprocess.run(
            self.argv(actor, *rest),
            input=stdin,
            capture_output=True,
            text=True,
            timeout=30,
            env=self.cenv(actor),
        )
        if check and r.returncode != 0:
            raise RuntimeError(f"{rest}: {r.stderr.strip()}")
        return r

    def rpc(self, actor, *rest):
        return json.loads(self.run(actor, "--json", *rest).stdout)

    def publish(self, to, body, priority=2):
        """Returns (card id, time the publisher's send returned)."""
        out = self.rpc("pub", "send", to, body, "--ask", "-p", str(priority))
        return out["card"]["id"], now()

    def drain(self, actor):
        """Handle everything pending for actor, exactly as shown."""
        for _ in range(10):
            page = self.rpc(actor, "inbox", "--selection", "all")
            if not page.get("items"):
                return
            self.run(actor, "ack", "--batch", page["batch"])

    def close(self):
        self.server.terminate()
        try:
            self.server.wait(10)
        except subprocess.TimeoutExpired:
            self.server.kill()
        self.log.close()


def trial_wait(b, i):
    proc = subprocess.Popen(
        b.argv("sub", "--json", "wait", "--new", "--timeout", str(int(args.timeout))),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=b.cenv("sub"),
    )
    try:
        time.sleep(args.settle)
        card, sent = b.publish("sub", f"wait trial {i}", priority=args.priority)
        try:
            out, _ = proc.communicate(timeout=args.timeout)
        except subprocess.TimeoutExpired:
            return {"ok": False, "reason": "timeout"}
        arrived = now()
        ids = [item["card"]["id"] for item in json.loads(out or "{}").get("items", [])]
        if card not in ids:
            return {"ok": False, "reason": f"woke without card {card}: {ids}"}
        return {"ok": True, "ns": arrived - sent}
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        b.drain("sub")


def notice_ids(line):
    """Card ids named by one attention notice line."""
    try:
        notice = json.loads(line)
    except ValueError:
        return set()
    return {t.get("id") for t in notice.get("titles", []) if isinstance(t, dict)}


class Watch:
    """One long-running attention stream, as a host monitor runs it."""

    def __init__(self, b):
        self.b = b
        self.lines = queue.Queue()
        self.proc = subprocess.Popen(
            b.argv(
                "sub",
                "watch",
                "--attention",
                "--notification",
                "--selection",
                "involved",
            ),
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            env=b.cenv("sub"),
        )
        threading.Thread(target=self.read, daemon=True).start()
        time.sleep(max(args.settle, 0.5))

    def read(self):
        for line in self.proc.stdout:
            self.lines.put((now(), line))

    def trial(self, i):
        while not self.lines.empty():
            self.lines.get_nowait()
        card, sent = self.b.publish("sub", f"watch trial {i}", priority=args.priority)
        deadline = time.monotonic() + args.timeout
        seen = 0
        arrived = None
        while time.monotonic() < deadline:
            try:
                t, line = self.lines.get(timeout=max(0.01, deadline - time.monotonic()))
            except queue.Empty:
                break
            if card in notice_ids(line):
                seen += 1
                arrived = arrived or t
                # Collect any duplicate that follows closely, then stop.
                deadline = min(deadline, time.monotonic() + 0.2)
        self.b.drain("sub")
        if arrived is None:
            return {"ok": False, "reason": "timeout"}
        return {"ok": True, "ns": arrived - sent, "duplicates": seen - 1}

    def close(self):
        self.proc.terminate()
        try:
            self.proc.wait(5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


STUB = r"""
import json, os, subprocess, sys, time
started = time.monotonic_ns()
packet = sys.stdin.read()
with open(sys.argv[1], "a") as f:
    f.write(json.dumps({"started": started, "read": time.monotonic_ns(), "bytes": len(packet), "packet": packet[:4000]}) + "\n")
# Handle what was shown, as a worker would, so the runner sees progress.
fray = sys.argv[2:]
page = json.loads(subprocess.run(fray + ["--json", "inbox", "--selection", "all"], capture_output=True, text=True).stdout or "{}")
if page.get("items"):
    subprocess.run(fray + ["ack", "--batch", page["batch"]], capture_output=True)
"""


def trial_drive(b, i, stub, record):
    open(record, "w").close()
    proc = subprocess.Popen(
        b.argv(
            "sub",
            "drive",
            "--max-turns",
            "1",
            "--idle-timeout",
            str(int(args.timeout)),
            "--child-timeout",
            "30",
            "--debounce-ms",
            "0",
            "--",
            sys.executable,
            stub,
            record,
            binary,
            "--home",
            b.home,
            "--as",
            "sub",
        ),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        env=b.cenv("sub"),
    )
    try:
        time.sleep(max(args.settle, 0.5))
        card, sent = b.publish("sub", f"drive trial {i}", priority=args.priority)
        try:
            proc.wait(args.timeout + 5)
        except subprocess.TimeoutExpired:
            return {"ok": False, "reason": "timeout"}
        rows = [json.loads(line) for line in open(record) if line.strip()]
        if not rows:
            return {
                "ok": False,
                "reason": "child never ran: " + proc.stderr.read()[-300:],
            }
        if (
            f"#{card}" not in rows[0]["packet"]
            and f'"id":{card}' not in rows[0]["packet"]
        ):
            return {"ok": False, "reason": f"packet lacked card {card}"}
        return {
            "ok": True,
            "ns": rows[0]["started"] - sent,
            "packet_ns": rows[0]["read"] - sent,
            "packet_bytes": rows[0]["bytes"],
        }
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        b.drain("sub")


def hook_ids(stdout):
    """Card ids in the attention a hook injected as additional context."""
    try:
        context = json.loads(stdout)["hookSpecificOutput"]["additionalContext"]
        data = json.loads(context.split("\n", 1)[1])
    except (ValueError, KeyError, IndexError):
        return set()
    return {item["card"]["id"] for item in data.get("attention", {}).get("items", [])}


def trial_hook(b, i):
    # Mid-turn injection deliberately surfaces only fresh, unresolved p0-p1
    # items addressed to the agent; lower priorities wait for the next brief.
    card, sent = b.publish("sub", f"hook trial {i}", priority=1)
    payload = json.dumps(
        {
            "hook_event_name": "PostToolUse",
            "session_id": "bench-hook",
            "tool_name": "Bash",
        }
    )
    env = b.cenv("sub")
    env["FRAY_AGENT"] = "sub"
    started = now()
    r = subprocess.run(
        [binary, "--home", b.home, "hook"],
        input=payload,
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )
    done = now()
    b.drain("sub")
    if r.returncode != 0:
        return {"ok": False, "reason": r.stderr.strip()[-300:]}
    if card not in hook_ids(r.stdout):
        return {"ok": False, "reason": f"hook context lacked card {card}"}
    # Publication to returned context, given a tool boundary right after publish.
    return {"ok": True, "ns": done - sent, "hook_ns": done - started}


def main():
    home = tempfile.mkdtemp(prefix="fwb-", dir=args.home_parent)
    b = Board(home)
    results = {}
    try:
        b.run("pub", "join")
        b.run("sub", "join")
        stub = os.path.join(home, "stub.py")
        pathlib.Path(stub).write_text(STUB)
        for path in paths:
            trials = []
            watch = Watch(b) if path == "watch" else None
            try:
                for i in range(args.n):
                    if path == "wait":
                        trials.append(trial_wait(b, i))
                    elif path == "watch":
                        trials.append(watch.trial(i))
                    elif path == "drive":
                        trials.append(
                            trial_drive(b, i, stub, os.path.join(home, "drive.jsonl"))
                        )
                    else:
                        trials.append(trial_hook(b, i))
            finally:
                if watch:
                    watch.close()
            ok = [t for t in trials if t["ok"]]
            summary = {
                "trials": len(trials),
                "delivered": len(ok),
                "failed": len(trials) - len(ok),
                "failures": sorted({t["reason"] for t in trials if not t["ok"]})[:5],
                "duplicates": sum(t.get("duplicates", 0) for t in ok),
                "publish_to_host": distribution([t["ns"] for t in ok]),
            }
            if path == "drive":
                summary["publish_to_packet_read"] = distribution(
                    [t["packet_ns"] for t in ok]
                )
                summary["packet_bytes_max"] = max(
                    (t["packet_bytes"] for t in ok), default=None
                )
            if path == "hook":
                summary["hook_exec"] = distribution([t["hook_ns"] for t in ok])
            results[path] = summary
    finally:
        b.close()
    version = subprocess.run(
        [binary, "--version"], capture_output=True, text=True
    ).stdout.strip()
    out = {
        "fray": version,
        "platform": f"{platform.system()} {platform.release()} {platform.machine()}",
        "python": platform.python_version(),
        "trials_per_path": args.n,
        "priority": args.priority,
        "settle_s": args.settle,
        "timeout_s": args.timeout,
        "measures": "publisher send returned -> host-visible event; excludes idle host scheduling and model response",
        "paths": results,
    }
    if args.json:
        print(json.dumps(out, indent=2))
        return
    print(f"{version} on {out['platform']}; {args.n} trials per path")
    print(f"{'path':<6} {'ok':>7} {'dup':>4} {'p50 ms':>8} {'p95 ms':>8} {'max ms':>8}")
    for path, s in results.items():
        d = s["publish_to_host"]
        print(
            f"{path:<6} {s['delivered']:>3}/{s['trials']:<3} {s['duplicates']:>4} {d['p50_ms']!s:>8} {d['p95_ms']!s:>8} {d['max_ms']!s:>8}"
        )
        for reason in s["failures"]:
            print(f"       failure: {reason}")
    print(json.dumps(out))


if __name__ == "__main__":
    main()
