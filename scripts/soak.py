#!/usr/bin/env python3
"""Bounded local Fray daemon soak; writes raw samples and a qualification report."""
import argparse, hashlib, json, pathlib, shutil, socket, subprocess, sys, tempfile, time

DEFAULTS = {"agents": 30, "warmup": 60, "duration": 7200, "fd_limit": 256,
            "rss_mib_limit": 256, "wal_mib_limit": 64, "p95_publish_ms_limit": 100}

class Peer:
    def __init__(self, home):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(5)
        self.socket.connect(str(pathlib.Path(home) / "bus.sock"))
        self.file = self.socket.makefile("rwb", buffering=0)
    def call(self, op, actor="", args=None):
        self.file.write((json.dumps({"op": op, "actor": actor, "args": args or {}}) + "\n").encode())
        return self.receive()
    def receive(self):
        line = self.file.readline()
        if not line: raise EOFError("daemon closed persistent socket")
        reply = json.loads(line)
        if not reply.get("ok"): raise RuntimeError(reply)
        return reply["data"]
    def close(self):
        self.file.close(); self.socket.close()

def metric(pid, home):
    ps = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], text=True, capture_output=True)
    if ps.returncode or not ps.stdout.strip(): raise RuntimeError("RSS metric unavailable")
    lsof = subprocess.run(["lsof", "-p", str(pid), "-Fn"], text=True, capture_output=True)
    if lsof.returncode: raise RuntimeError("FD metric unavailable")
    wal = pathlib.Path(home, "state.db-wal")
    return {"time": time.time(), "rss_bytes": int(ps.stdout.strip()) * 1024,
            "fds": sum(1 for line in lsof.stdout.splitlines() if line.startswith("n")),
            "wal_bytes": wal.stat().st_size if wal.exists() else 0}

def percentile(values, p):
    if not values: return None
    return sorted(values)[min(len(values) - 1, int((len(values) - 1) * p))]

def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

def drain_watch(watcher, state, through):
    while state["cursor"] < through:
        frame = watcher.receive()
        if frame.get("type") == "checkpoint":
            if frame["cursor"] > state["cursor"]:
                raise RuntimeError("watch skipped event frames")
            continue
        if frame.get("type") != "event": raise RuntimeError("watch returned invalid frame")
        sequence = frame["event"]["seq"]
        if sequence in state["seen"]: raise RuntimeError("watch delivered duplicate event")
        if sequence != state["cursor"] + 1: raise RuntimeError("watch skipped event sequence")
        state["seen"].add(sequence); state["cursor"] = sequence

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", nargs="?", default="target/debug/fray")
    parser.add_argument("--out", required=True, type=pathlib.Path)
    for key, value in DEFAULTS.items(): parser.add_argument("--" + key.replace("_", "-"), type=int, default=value)
    args = parser.parse_args()
    if args.agents < 30 or args.warmup < 0 or args.duration < 1:
        parser.error("agents must be >=30, warmup >=0, and duration >=1")
    args.out.mkdir(parents=True, exist_ok=True)
    config = {key: str(value) if isinstance(value, pathlib.Path) else value for key, value in vars(args).items()}
    binary = pathlib.Path(args.binary).resolve()
    source_sha = subprocess.run(["git", "rev-parse", "HEAD"], text=True, capture_output=True)
    report = {"defaults": DEFAULTS, "config": config, "success": False, "qualified": False, "timeouts": 0,
              "duplicates": 0, "sqlite_busy": 0, "samples": 0, "failure": None}
    samples_path = args.out / "samples.jsonl"
    home = tempfile.mkdtemp(prefix="fray-soak-", dir="/tmp")
    daemon = None; peers = []; watchers = []
    try:
        daemon = subprocess.Popen([str(binary), "--home", home, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        report["owned"] = {"pid": daemon.pid, "home": home, "binary_sha256": sha256(binary),
                           "source_sha": source_sha.stdout.strip() if source_sha.returncode == 0 else None}
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                probe = Peer(home); probe.call("ping"); probe.close(); break
            except (OSError, EOFError): time.sleep(.02)
        else: raise RuntimeError("daemon did not become ready")
        peers = [Peer(home) for _ in range(args.agents)]
        names = [f"soak-{i:02}" for i in range(args.agents)]
        for peer, name in zip(peers, names): peer.call("join", name)
        watchers = [Peer(home) for _ in range(min(4, args.agents))]
        watch_state = []
        for watcher, name in zip(watchers, names):
            ready = watcher.call("watch", name, {"after": 0})
            if ready.get("type") != "ready": raise RuntimeError("watch did not become ready")
            watch_state.append({"cursor": ready["cursor"], "seen": set()})
        seen = set(); timings = []; started = time.monotonic(); next_sample = started
        with samples_path.open("w") as raw:
            while time.monotonic() - started < args.warmup + args.duration:
                index = int((time.monotonic() - started) * 10) % args.agents
                sender, receiver = names[index], names[(index + 1) % args.agents]
                begin = time.perf_counter()
                sent = peers[index].call("send", sender, {"to": receiver, "body": f"soak-{index}-{len(seen)}"})
                elapsed = (time.perf_counter() - begin) * 1000
                card = sent["card"]["id"]
                if card in seen: report["duplicates"] += 1
                seen.add(card)
                for watcher, state in zip(watchers, watch_state): drain_watch(watcher, state, sent["event_seq"])
                inbox = peers[(index + 1) % args.agents].call("inbox", receiver, {"selection": "all"})
                receipts = [item["receipt"] for item in inbox["items"] if item["card"]["id"] == card and item["receipt"]["through_seq"] == sent["event_seq"]]
                if len(receipts) != 1: raise RuntimeError("receiver inbox omitted fresh exact delivery")
                peers[(index + 1) % args.agents].call("ack", receiver, {"receipts": receipts})
                peers[index].call("wait", sender, {"timeout": 0})
                measured = time.monotonic() - started >= args.warmup
                if measured: timings.append(elapsed)
                if time.monotonic() >= next_sample:
                    sample = metric(daemon.pid, home); sample["phase"] = "measure" if measured else "warmup"
                    raw.write(json.dumps(sample) + "\n"); raw.flush()
                    if measured: report["samples"] += 1
                    next_sample += 1
        report["publish_ms"] = {"p50": percentile(timings, .50), "p95": percentile(timings, .95), "max": max(timings, default=None)}
        report["limits"] = {"fd": args.fd_limit, "rss_bytes": args.rss_mib_limit * 1024**2, "wal_bytes": args.wal_mib_limit * 1024**2, "p95_publish_ms": args.p95_publish_ms_limit}
        raw_samples = [json.loads(line) for line in samples_path.read_text().splitlines() if json.loads(line)["phase"] == "measure"]
        qualified = bool(raw_samples and timings and report["duplicates"] == report["timeouts"] == report["sqlite_busy"] == 0
                         and max(x["fds"] for x in raw_samples) <= args.fd_limit
                         and max(x["rss_bytes"] for x in raw_samples) <= args.rss_mib_limit * 1024**2
                         and max(x["wal_bytes"] for x in raw_samples) <= args.wal_mib_limit * 1024**2
                         and report["publish_ms"]["p95"] <= args.p95_publish_ms_limit)
        report["success"] = qualified
        report["qualified"] = qualified and args.warmup >= DEFAULTS["warmup"] and args.duration >= DEFAULTS["duration"]
        if not qualified: report["failure"] = "qualification threshold exceeded or metrics unavailable"
        elif not report["qualified"]: report["failure"] = "short smoke completed; default 60s warmup and 7200s duration are required for qualification"
    except socket.timeout as error:
        report["timeouts"] += 1; report["failure"] = str(error)
    except Exception as error:
        if "SQLITE_BUSY" in str(error).upper() or "DATABASE IS LOCKED" in str(error).upper(): report["sqlite_busy"] += 1
        report["failure"] = str(error)
    finally:
        for peer in peers:
            try: peer.close()
            except OSError: pass
        for watcher in watchers:
            try: watcher.close()
            except OSError: pass
        if daemon and daemon.poll() is None:
            daemon.terminate()
            try: daemon.wait(timeout=3)
            except subprocess.TimeoutExpired: daemon.kill(); daemon.wait()
        shutil.rmtree(home, ignore_errors=True)
        (args.out / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps(report, sort_keys=True))
    return 0 if report["success"] else 1

if __name__ == "__main__": sys.exit(main())
