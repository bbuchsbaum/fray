"""Real Fray receipt/lifecycle regressions; Codex is an isolated WebSocket mock."""
import asyncio
from contextlib import asynccontextmanager
import json
import os
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from websockets.asyncio.server import unix_serve

import wake
from wake import Journal, MAX_RECEIPTS, deliver, run
from test_wake import FakeRpc

ROOT = Path(__file__).resolve().parents[2]
BINARY = Path(os.environ.get("FRAY_WAKE_TEST_BINARY", ROOT / "target/debug/fray")).resolve()
sys.path.insert(0, str(ROOT / "scripts"))
with patch.object(sys, "argv", ["integration.py", str(BINARY)]):
    import integration


class RealFrayTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.fray = integration.Integration()
        self.fray.setUp()
        self.addCleanup(self.fray.tearDown)
        self.fray.call("join", "manager")
        self.store = self.fray.call("ping")["store_id"]
        self.journal = Journal(Path(self.fray.home) / "journal.json", {"thread": "thread"})
        self.addCleanup(self.journal.lock.close)
        self.config = SimpleNamespace(thread="thread", cwd="/tmp", home=self.fray.home,
                                      agent="manager", fray=str(BINARY), session="session",
                                      socket=str(Path(self.fray.home) / "codex.sock"),
                                      selection="all", lifetime=10, max_deliveries=1, probe=False)

    def queue_receipt(self, n):
        sent = self.fray.call("send", "alice", {"to": "manager", "body": f"Review item {n}"})
        receipt = {"store_id": self.store, "agent": "manager", "id": sent["card"]["id"],
                   "through_seq": sent["event_seq"]}
        batch = self.fray.call("present", "manager", {"source": "attention", "receipts": [receipt]})["batch"]["id"]
        self.journal.add({"type": "attention", "packet_version": 1, "store_id": self.store,
                          "agent": "manager", "batch": batch, "items": [{"receipt": receipt}]}, "manager")
        return receipt, batch

    @asynccontextmanager
    async def codex(self, before_response=None):
        calls = []

        async def server(ws):
            async for raw in ws:
                request = json.loads(raw)
                if "id" not in request:
                    continue
                method = request["method"]
                calls.append(method)
                if before_response:
                    await before_response(method, calls)
                result = ({"thread": {"id": "thread", "cwd": "/tmp", "status": {"type": "idle"}}}
                          if method == "thread/read" else {"turn": {"id": "wake-turn"}}
                          if method == "turn/start" else {})
                await ws.send(json.dumps({"id": request["id"], "result": result}))

        async with unix_serve(server, self.config.socket):
            yield calls

    async def test_recovered_delivery_does_not_wake_after_leave(self):
        self.queue_receipt(0)
        self.fray.call("leave", "manager")
        async with self.codex() as calls:
            with self.assertRaisesRegex(RuntimeError, "Fray watcher exited"):
                await asyncio.wait_for(run(self.config, self.journal), 5)
        self.assertNotIn("turn/start", calls)
        self.assertEqual(self.journal.pending()["status"], "pending")

    async def test_leave_during_host_read_prevents_dispatch(self):
        self.queue_receipt(0)
        stopped = asyncio.Event()
        original_watch = wake.watch

        async def observed_watch(*args):
            try:
                return await original_watch(*args)
            finally:
                stopped.set()

        async def leave_after_read(method, calls):
            if method == "thread/read" and calls.count(method) == 2:
                self.fray.call("leave", "manager")
                # Hold the independent host response until Fray has closed its
                # listener. This deterministically exercises the await boundary.
                await asyncio.wait_for(stopped.wait(), 3)

        with patch.object(wake, "watch", observed_watch):
            async with self.codex(leave_after_read) as calls:
                with self.assertRaisesRegex(RuntimeError, "Fray watcher exited"):
                    await asyncio.wait_for(run(self.config, self.journal), 5)
        self.assertNotIn("turn/start", calls)
        self.assertEqual(self.journal.pending()["status"], "pending")

    async def test_real_ready_listener_delivers_and_is_reaped(self):
        self.queue_receipt(0)
        async with self.codex() as calls:
            await asyncio.wait_for(run(self.config, self.journal), 5)
        self.assertEqual(calls.count("turn/start"), 1)
        self.assertEqual(self.journal.data["deliveries"][0]["status"], "delivered")
        self.assertEqual(self.fray.call("inbox", "manager")["total"], 1)
        agent = next(a for a in self.fray.call("agents")["items"] if a["name"] == "manager")
        self.assertEqual(agent["listener"]["state"], "stopped")

    async def test_expired_batches_still_leave_actionable_exact_receipts(self):
        packets = [self.queue_receipt(n) for n in range(40)]
        with integration.Peer(self.fray.home) as peer:
            expired = peer.call("batch", "manager", {"batch": packets[0][1]})
        self.assertEqual(expired["error"]["code"], "batch_unknown")
        pending = self.journal.pending()
        rpc = FakeRpc()
        self.assertTrue(await deliver(rpc, self.config, self.journal, pending, lambda: True))
        text = rpc.calls[-1][1]["input"][0]["text"]
        # Parse the actual model-visible receipts and use them against Fray.
        receipts = json.loads(text.split("tokens): ", 1)[1].split("\n", 1)[0])
        self.assertEqual(receipts, [r for r, _ in packets[:MAX_RECEIPTS]])
        self.assertEqual(self.fray.call("inbox", "manager")["total"], 40)
        for receipt in receipts:
            shown = self.fray.call("show", "manager", {"id": receipt["id"], "unread": True})
            self.assertEqual(shown["card"]["id"], receipt["id"])
        # A later update must not be swallowed by handling the old wake.
        self.fray.call("annotate", "alice", {"id": receipts[0]["id"], "kind": "note",
                                               "body": "Later unread update"})
        self.fray.call("ack", "manager", {"receipts": receipts})
        self.assertEqual(self.fray.call("inbox", "manager")["total"], 40 - MAX_RECEIPTS + 1)


if __name__ == "__main__":
    unittest.main()
