#!/usr/bin/env python3
"""Measure end-to-end response through Fray with real model hosts.

Epic child 3 (Mote bd-01M3RZ9BN4RYQRY56RFEPSTEJX): the model-response column
that scripts/bench_wake.py leaves out. Owner-approved on 2026-09-30 as a
bounded run: every trial is one short model turn.

Each trial:
  1. `fray drive --max-turns 1` waits as agent `sub`, with a real host as its
     child (`claude -p` or `codex exec -`), started through a tiny wrapper
     that timestamps the host's start.
  2. The publisher sends a p1 question to sub asking for the reply `pong`.
  3. The publisher's own `fray wait --card ID` returns when sub replies.

Reported per host, on one monotonic clock, from the publisher's send
returning:
  t_host_started  the drive runner started the host process (scheduling)
  t_reply         sub's reply reached the publisher (model response included)
Failures and timeouts count against a host; they are never dropped.
"""
import argparse
import json
import os
import pathlib
import platform
import shutil
import subprocess
import sys
import tempfile
import time

parser = argparse.ArgumentParser(
    description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
)
parser.add_argument("binary", nargs="?", default="target/release/fray")
parser.add_argument("--hosts", default="claude,codex")
parser.add_argument(
    "--n", type=int, default=5, help="trials per host (declared up front)"
)
parser.add_argument(
    "--timeout",
    type=float,
    default=240.0,
    help="seconds before a trial counts as failed",
)
parser.add_argument("--home-parent", default="/tmp")
parser.add_argument("--json", action="store_true")
args = parser.parse_args()
binary = str(pathlib.Path(args.binary).resolve())
if not pathlib.Path(binary).is_file():
    parser.error("build first, e.g. cargo build --release")
if not 1 <= args.n <= 20:
    parser.error("--n must be in 1..20: every trial is a paid model turn")

HOSTS = {
    # Non-interactive Claude Code, allowed only to run fray.
    "claude": ["claude", "-p", "--allowedTools", "Bash(fray:*)"],
    # Non-interactive Codex, sandboxed to the trial directory (the board's
    # socket lives inside it).
    "codex": [
        "codex",
        "exec",
        "--skip-git-repo-check",
        "--sandbox",
        "workspace-write",
        "-",
    ],
}
hosts = [h for h in args.hosts.split(",") if h]
for h in hosts:
    if h not in HOSTS:
        parser.error(f"unknown host {h}")

now = time.monotonic_ns
WRAPPER = r"""
import os, sys, time
with open(sys.argv[1], "a") as f:
    f.write(str(time.monotonic_ns()) + "\n")
os.execvp(sys.argv[2], sys.argv[2:])
"""


def distribution(xs):
    if not xs:
        return {"n": 0, "p50_s": None, "p95_s": None, "max_s": None}
    xs = sorted(xs)

    def rank(q):
        return round(
            xs[min(len(xs) - 1, max(0, -(-int(q * len(xs) * 1000) // 1000) - 1))] / 1e9,
            2,
        )

    return {
        "n": len(xs),
        "p50_s": rank(0.5),
        "p95_s": rank(0.95),
        "max_s": round(xs[-1] / 1e9, 2),
    }


def main():
    root = tempfile.mkdtemp(prefix="fbm-", dir=args.home_parent)
    home = os.path.join(root, ".fray")
    env = {k: v for k, v in os.environ.items() if not k.startswith(("FRAY_", "MOTE_"))}
    env["FRAY_HOME"] = home
    # The host's own fray calls must reach this board with this build.
    env["PATH"] = os.path.dirname(binary) + os.pathsep + env.get("PATH", "")

    def fray(actor, *rest, check=True, timeout=30):
        e = dict(env)
        e["FRAY_SESSION"] = f"bench:{actor}"
        r = subprocess.run(
            [binary, "--home", home, "--as", actor, *rest],
            capture_output=True,
            text=True,
            timeout=timeout,
            env=e,
            cwd=root,
        )
        if check and r.returncode != 0:
            raise RuntimeError(f"{rest}: {r.stderr.strip()}")
        return r

    os.makedirs(home)
    wrapper = os.path.join(root, "stamp.py")
    pathlib.Path(wrapper).write_text(WRAPPER)
    fray("pub", "start")
    results = {}
    try:
        fray("pub", "join")
        fray("sub", "join")
        for host in hosts:
            if not shutil.which(HOSTS[host][0]):
                results[host] = {"skipped": f"{HOSTS[host][0]} not on PATH"}
                continue
            trials = []
            for i in range(args.n):
                stamps = os.path.join(root, f"{host}-{i}.stamp")
                e = dict(env)
                e["FRAY_SESSION"] = "bench:sub"
                log = open(os.path.join(root, f"{host}-{i}.log"), "w")
                drive = subprocess.Popen(
                    [
                        binary,
                        "--home",
                        home,
                        "--as",
                        "sub",
                        "drive",
                        "--max-turns",
                        "1",
                        "--idle-timeout",
                        str(int(args.timeout)),
                        "--child-timeout",
                        str(int(args.timeout)),
                        "--debounce-ms",
                        "0",
                        "--",
                        sys.executable,
                        wrapper,
                        stamps,
                        *HOSTS[host],
                    ],
                    stdout=log,
                    stderr=log,
                    env=e,
                    cwd=root,
                )
                try:
                    time.sleep(1.0)
                    cursor = json.loads(fray("pub", "--json", "ping").stdout)["cursor"]
                    sent_out = fray(
                        "pub",
                        "--json",
                        "send",
                        "sub",
                        f"Benchmark ping {i}. Reply to this card with exactly: pong. "
                        "Use `fray reply CARD_ID pong` with this card's id, then stop. Do nothing else.",
                        "--ask",
                        "-p",
                        "1",
                    )
                    sent = now()
                    card = json.loads(sent_out.stdout)["card"]["id"]
                    waited = fray(
                        "pub",
                        "--json",
                        "wait",
                        "--card",
                        str(card),
                        "--after",
                        str(cursor),
                        "--timeout",
                        str(int(args.timeout)),
                        
                        check=False,
                        timeout=args.timeout + 30,
                    )
                    replied = now()
                    # A reply to one's own question is not "addressed to me"
                    # (that filter means assigned to me), so the wait is unfiltered;
                    # a timed-out wait exits 0 with timed_out set.
                    try:
                        result = json.loads(waited.stdout or "{}")
                    except ValueError:
                        result = {}
                    answered = not result.get("timed_out") and any(
                        a.get("actor") == "sub"
                        for item in result.get("items", [])
                        if item.get("card", {}).get("id") == card
                        for a in item.get("annotations", [])
                    )
                    started = None
                    if os.path.exists(stamps):
                        with open(stamps) as f:
                            first = f.readline().strip()
                            started = int(first) if first else None
                    if waited.returncode != 0 or not answered:
                        trials.append(
                            {
                                "ok": False,
                                "reason": "no reply within timeout"
                                if waited.returncode
                                else "woke without sub's reply",
                                "host_started_s": None
                                if started is None
                                else round((started - sent) / 1e9, 3),
                            }
                        )
                    else:
                        trials.append(
                            {
                                "ok": True,
                                "reply_ns": replied - sent,
                                "host_ns": None if started is None else started - sent,
                            }
                        )
                finally:
                    try:
                        drive.wait(args.timeout + 60)
                    except subprocess.TimeoutExpired:
                        drive.kill()
                        drive.wait()
                    log.close()
                    # Handle what the publisher was shown so the next trial starts clean.
                    for who in ("pub", "sub"):
                        page = json.loads(
                            fray(who, "--json", "inbox", "--selection", "all").stdout
                        )
                        if page.get("items"):
                            fray(who, "ack", "--batch", page["batch"])
            ok = [t for t in trials if t["ok"]]
            results[host] = {
                "trials": len(trials),
                "answered": len(ok),
                "failed": len(trials) - len(ok),
                "failures": [t for t in trials if not t["ok"]][:5],
                "publish_to_host_started": distribution(
                    [t["host_ns"] for t in ok if t["host_ns"] is not None]
                ),
                "publish_to_reply": distribution([t["reply_ns"] for t in ok]),
            }
    finally:
        fray("pub", "stop", check=False)
    out = {
        "fray": subprocess.run(
            [binary, "--version"], capture_output=True, text=True
        ).stdout.strip(),
        "hosts": {
            h: subprocess.run(
                [HOSTS[h][0], "--version"], capture_output=True, text=True
            ).stdout.strip()
            for h in hosts
            if shutil.which(HOSTS[h][0])
        },
        "platform": f"{platform.system()} {platform.release()} {platform.machine()}",
        "trials_per_host": args.n,
        "timeout_s": args.timeout,
        "measures": "publisher send returned -> host process started, and -> sub's reply seen by the publisher's wait",
        "results": results,
        "artifacts": root,
    }
    print(json.dumps(out, indent=None if args.json else 2))


if __name__ == "__main__":
    main()
