"""Deterministic stdin agent for runner tests; no model or network service."""
import json
import os
from pathlib import Path
import socket
import sys
import time

prompt = sys.stdin.read()
state = json.loads(prompt.split("CURRENT PROJECT STATE\n", 1)[1])
assert state["agent"] == os.environ["FRAY_AGENT"]
state["prompt_bytes"] = len(prompt.encode())
if "--sleep" in sys.argv:
    time.sleep(float(sys.argv[sys.argv.index("--sleep") + 1]))

if "--ignore" not in sys.argv:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
        peer.settimeout(5)
        peer.connect(os.environ["FRAY_HOME"] + "/bus.sock")
        with peer.makefile("rwb", buffering=0) as stream:
            items = state["attention"]["items"]
            if "--partial" in sys.argv:
                items = items[:1]
            for item in items:
                request = {
                    "op": "ack",
                    "actor": state["agent"],
                    "args": {"receipts": [item["receipt"]]},
                }
                stream.write((json.dumps(request) + "\n").encode())
                assert json.loads(stream.readline())["ok"]

with Path(sys.argv[1]).open("a") as record:
    record.write(json.dumps(state) + "\n")
