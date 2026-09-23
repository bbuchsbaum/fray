#!/usr/bin/env python3
"""Thin Claude lifecycle adapter; selection/packets/leases belong to Fray.

monitor: replace this process with the host-neutral persistent NDJSON consumer.
rewake: one-shot asyncRewake hook; success maps to Claude's exit code 2.
No automatic joining, daemon launch, acknowledgments, or permission changes.
"""
import json
import os
import signal
import subprocess
import sys


def main():
    mode = sys.argv[1] if len(sys.argv) == 2 else ""
    if mode not in ("monitor", "rewake"):
        print("usage: attention.py monitor|rewake", file=sys.stderr)
        return 1
    # Loading the plugin outside an explicit Fray session is inert. A managed
    # drive child already has its own packet and must not start another consumer.
    if os.environ.get("FRAY_DRIVE") == "1":
        return 0
    home, agent = os.environ.get("FRAY_HOME"), os.environ.get("FRAY_AGENT")
    if not home or not agent:
        return 0
    command = [os.environ.get("FRAY_BIN", "fray"), "--home", home, "--as", agent,
               "watch", "--attention", "--selection",
               os.environ.get("FRAY_SELECTION", "involved"), "--reconnect"]
    if mode == "monitor":
        os.execvp(command[0], command)
    # This fallback is deliberately one-shot per SessionStart, not a Stop loop
    # that repeatedly wakes on an ignored receipt. Rearm explicitly as needed.
    payload = json.load(sys.stdin)
    if payload.get("hook_event_name") != "SessionStart":
        return 0
    child = subprocess.Popen(command + ["--once", "--timeout", "3300"],
                             stdout=subprocess.PIPE, text=True)
    def cancel(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, cancel)
    signal.signal(signal.SIGINT, cancel)
    try:
        output, _ = child.communicate()
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
    if child.returncode == 0:
        packet = json.loads(output)
        if packet.get("type") != "attention" or not packet.get("items"):
            raise ValueError("expected a nonempty Fray attention packet")
        print(output.rstrip(), file=sys.stderr, flush=True)
        return 2
    if child.returncode == 3:  # Quiet deadline; no model wakeup.
        return 0
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError) as error:
        print(f"fray Claude adapter: {error}", file=sys.stderr)
        sys.exit(1)
