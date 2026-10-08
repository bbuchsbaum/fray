#!/usr/bin/env python3
"""Measure end-to-end response through Fray with real model hosts.

Epic child 3 (Mote bd-01M3RZ9BN4RYQRY56RFEPSTEJX): the model-response column
that scripts/bench_wake.py leaves out. Owner-approved on 2026-09-30 as a
bounded run: every trial is one short model turn.

Each trial:
  1. `fray drive --max-turns 1` waits as agent `sub`, with a real host as its
     child (`claude -p` or `codex exec -`).
  2. The publisher sends a p1 question to sub asking for the reply `pong`.
  3. The publisher's own `fray wait --card ID` returns when sub replies.

Reported per host, from the publisher's send returning:
  t_host_started  when `fray drive` spawned the host, from the runner's own
                  record (wall clock, millisecond resolution)
  t_reply         sub's reply reached the publisher's wait (monotonic clock;
                  model response included). Any annotation by sub counts:
                  this is latency, not whether the model obeyed.
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
# Keep scratch daemons out of the user's daemon registry.
_STATE_DIR = tempfile.TemporaryDirectory(prefix="fray-state-")
os.environ["FRAY_STATE_DIR"] = _STATE_DIR.name

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
    # Non-interactive Claude Code with fray pre-approved (read-only tools
    # such as Read and Grep remain available).
    "claude": ["claude", "-p", "--allowedTools", "Bash(fray:*)"],
    # Non-interactive Codex, workspace-write sandbox rooted at the trial
    # directory, where the board's socket lives (writes to temp dirs are also
    # allowed).
    "codex": [
        "codex",
        "exec",
        "--skip-git-repo-check",
        "--sandbox",
        "workspace-write",
        "-",
    ],
}
# Testing the harness without paid turns: FRAY_BENCH_HOSTS_JSON maps a host
# name to a replacement argv, e.g. {"claude": ["sh", "stub.sh"]}.
if os.environ.get("FRAY_BENCH_HOSTS_JSON"):
    HOSTS.update(json.loads(os.environ["FRAY_BENCH_HOSTS_JSON"]))
hosts = [h for h in args.hosts.split(",") if h]
for h in hosts:
    if h not in HOSTS:
        parser.error(f"unknown host {h}")

now = time.monotonic_ns


def distribution(xs):
    if not xs:
        return {"n": 0, "p50_s": None, "p95_s": None, "max_s": None}
    xs = sorted(xs)

    def rank(q):
        return round(
            xs[min(len(xs) - 1, max(0, -(-int(q * len(xs) * 1000) // 1000) - 1))] / 1e9,
            3,
        )

    return {
        "n": len(xs),
        "p50_s": rank(0.5),
        "p95_s": rank(0.95),
        "max_s": round(xs[-1] / 1e9, 3),
    }


def main():
    root = tempfile.mkdtemp(prefix="fbm-", dir=args.home_parent)
    home = os.path.join(root, ".fray")
    env = {k: v for k, v in os.environ.items() if not k.startswith(("FRAY_", "MOTE_"))}
    env["FRAY_STATE_DIR"] = os.environ["FRAY_STATE_DIR"]
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

    def drive_started_ms(log_path):
        """When `fray drive` spawned the host, from its own run record."""
        try:
            with open(log_path) as f:
                for line in f:
                    if line.startswith("fray drive: {"):
                        rec = json.loads(line[len("fray drive: "):])
                        if rec.get("child", {}).get("started_ms"):
                            return rec["child"]["started_ms"]
        except (OSError, ValueError):
            pass
        return None

    def trial(host, i):
        """One model turn. Any failure is recorded, never raised, so a
        declared run always reports every trial."""
        e = dict(env)
        e["FRAY_SESSION"] = "bench:sub"
        log_path = os.path.join(root, f"{host}-{i}.log")
        log = open(log_path, "w")
        drive = subprocess.Popen(
            [binary, "--home", home, "--as", "sub", "drive", "--max-turns", "1",
             "--idle-timeout", str(int(args.timeout)), "--child-timeout", str(int(args.timeout)),
             "--debounce-ms", "0", "--", *HOSTS[host]],
            stdout=log, stderr=log, env=e, cwd=root,
            # Its own session keeps a terminal Ctrl-C from reaching the drive
            # and host directly; the handler below stops them on purpose. The
            # drive puts the host in a separate group and stops it through its
            # watchdog when the drive itself exits (src/driver.rs).
            start_new_session=True,
        )
        outcome = {"ok": False, "reason": "not run"}
        interrupted = None
        try:
            time.sleep(1.0)
            cursor = json.loads(fray("pub", "--json", "ping").stdout)["cursor"]
            sent_out = fray(
                "pub", "--json", "send", "sub",
                f"Benchmark ping {i}. Reply to this card with exactly: pong. "
                "Use `fray reply CARD_ID pong` with this card's id, then stop. Do nothing else.",
                "--ask", "-p", "1",
            )
            sent = now()
            sent_wall_ms = time.time_ns() // 1_000_000
            card = json.loads(sent_out.stdout)["card"]["id"]
            # Unfiltered: a reply to one's own question is not "addressed to
            # me" (that filter means assigned to me). A timed-out wait exits 3.
            waited = fray(
                "pub", "--json", "wait", "--card", str(card), "--after", str(cursor),
                "--timeout", str(int(args.timeout)), check=False, timeout=args.timeout + 30,
            )
            replied = now()
            try:
                result = json.loads(waited.stdout or "{}")
            except ValueError:
                result = {}
            # Any annotation by sub on this card counts as the response: this
            # measures latency, not whether the model obeyed ("pong").
            answered = (waited.returncode == 0 and not result.get("timed_out") and any(
                a.get("actor") == "sub"
                for item in result.get("items", [])
                if item.get("card", {}).get("id") == card
                for a in item.get("annotations", [])
            ))
            if answered:
                outcome = {"ok": True, "reply_ns": replied - sent}
            else:
                outcome = {"ok": False, "reason": "no reply within timeout" if waited.returncode else "woke without sub's reply"}
            outcome["sent_wall_ms"] = sent_wall_ms
        except Exception as exc:  # recorded, never fatal to the run
            outcome = {"ok": False, "reason": f"{type(exc).__name__}: {exc}"}
        except BaseException as exc:  # Ctrl-C: stop the paid host now, then leave
            interrupted = exc
            outcome = {"ok": False, "reason": "interrupted"}
        finally:
            def stop_drive():
                # SIGTERM the drive; its watchdog then stops the host's group.
                for sig in (15, 9):
                    try:
                        os.killpg(drive.pid, sig)
                    except ProcessLookupError:
                        return
                    except OSError as err:
                        # A sandbox may refuse signals; report instead of
                        # raising, and fall back to the drive's own timeouts.
                        print(f"bench: could not signal drive {drive.pid}: {err}", file=sys.stderr)
                        return
                    try:
                        drive.wait(15)
                        return
                    except subprocess.TimeoutExpired:
                        continue

            if interrupted is not None:
                stop_drive()
            else:
                try:
                    drive.wait(args.timeout + 60)
                except subprocess.TimeoutExpired:
                    stop_drive()
            log.close()
            started = drive_started_ms(log_path)
            if started is not None and "sent_wall_ms" in outcome:
                outcome["host_ms"] = started - outcome["sent_wall_ms"]
            # Handle what each side was shown so the next trial starts clean.
            for who in ("pub", "sub"):
                try:
                    page = json.loads(fray(who, "--json", "inbox", "--selection", "all").stdout)
                    if page.get("items"):
                        fray(who, "ack", "--batch", page["batch"])
                except Exception:
                    pass
        if interrupted is not None:
            raise interrupted
        return outcome
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
                trials.append(trial(host, i))
            ok = [t for t in trials if t["ok"]]
            results[host] = {
                "trials": len(trials),
                "answered": len(ok),
                "failed": len(trials) - len(ok),
                "failures": [t for t in trials if not t["ok"]][:5],
                "publish_to_host_started": distribution(
                    [t["host_ms"] * 1_000_000 for t in ok if t.get("host_ms") is not None]
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
        "measures": "publisher send returned -> fray drive spawned the host (drive record, ms), and -> sub's reply seen by the publisher's wait",
        "results": results,
        "artifacts": root,
    }
    print(json.dumps(out, indent=None if args.json else 2))


if __name__ == "__main__":
    main()
