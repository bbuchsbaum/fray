import asyncio
import json
import os
from pathlib import Path
import tempfile
import sys
from types import SimpleNamespace
import unittest

from websockets.asyncio.server import unix_serve
from websockets.asyncio.client import unix_connect

from wake import MAX_RECEIPTS, Journal, Rejected, Rpc, deliver, target, run


def packet(seq=10, batch="b" * 32):
    receipt = {"store_id": "a" * 32, "agent": "manager", "id": 7, "through_seq": seq}
    return {"type": "attention", "packet_version": 1, "agent": "manager",
            "store_id": "a" * 32, "batch": batch, "items": [{"receipt": receipt}]}


class FakeRpc:
    def __init__(self, status="idle", flags=(), failure=None, turn_status="inProgress"):
        self.calls, self.failure = [], failure
        self.status = {"type": status, "activeFlags": list(flags)}
        self.turn_status = turn_status

    async def call(self, method, params):
        self.calls.append((method, params))
        if method == "thread/read":
            return {"thread": {"id": "thread", "cwd": "/tmp", "status": self.status}}
        if method == "thread/turns/list":
            return {"data": [{"id": "turn", "status": self.turn_status}]}
        if self.failure:
            raise self.failure
        return {"turnId": "turn"}


class AdapterTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "state.json"
        self.config = SimpleNamespace(thread="thread", cwd="/tmp", home="/tmp/fray", agent="manager",
                                      fray="fray", session="session")
        self.journal = Journal(self.path, {"thread": "thread"})
        self.journal.add(packet(), "manager")
        self.delivery = self.journal.pending()

    def tearDown(self):
        self.journal.lock.close()
        self.temp.cleanup()

    async def deliver(self, rpc, is_ready=lambda: True):
        return await deliver(rpc, self.config, self.journal, self.delivery, is_ready)

    def test_reconnect_dedup_uses_receipts_not_batches(self):
        self.assertFalse(self.journal.add(packet(batch="c" * 32), "manager"))
        self.assertTrue(self.journal.add(packet(seq=11), "manager"))

    def test_identity_and_store_changes_rejected(self):
        wrong = packet()
        wrong["store_id"] = "d" * 32
        with self.assertRaisesRegex(ValueError, "store changed"):
            self.journal.add(wrong, "manager")
        with self.assertRaisesRegex(ValueError, "identity"):
            self.journal.add(packet(), "other-agent")

    async def test_backlog_combines_exact_receipts_without_merging_delivered_entries(self):
        self.journal.add(packet(seq=11, batch="c" * 32), "manager")
        self.journal.add(packet(seq=12, batch="d" * 32), "manager")
        self.assertEqual(len(self.journal.data["deliveries"]), 1)
        rpc = FakeRpc()
        await self.deliver(rpc)
        prompt = rpc.calls[-1][1]["input"][0]["text"]
        for seq in (10, 11, 12):
            self.assertIn(f'"through_seq":{seq}', prompt)
        for batch in ("b" * 32, "c" * 32, "d" * 32):
            self.assertNotIn(batch, prompt)
        self.journal.add(packet(seq=13, batch="e" * 32), "manager")
        self.assertEqual(len(self.journal.data["deliveries"]), 2)
        self.assertEqual(len(self.delivery["keys"]), 3)

    def test_backlog_prompt_receipts_are_bounded(self):
        for i in range(1, MAX_RECEIPTS + 2):
            self.journal.add(packet(seq=10+i, batch=f"{i:032x}"), "manager")
        self.assertEqual([len(d["keys"]) for d in self.journal.data["deliveries"]], [MAX_RECEIPTS, 2])

    def test_legacy_pending_journal_is_split_without_redelivering_completed_entries(self):
        for seq in range(11, 80):
            self.journal.add(packet(seq=seq), "manager")
        deliveries = self.journal.data["deliveries"]
        keys = [key for d in deliveries for key in d["keys"]]
        legacy = {**deliveries[0], "keys": keys, "batch": "b" * 32}
        delivered = {"keys": [], "status": "delivered", "id": "retain-completed-id"}
        self.journal.data["deliveries"] = [delivered, legacy]
        self.journal.save()
        self.journal.lock.close()
        self.journal = Journal(self.path, {"thread": "thread"})
        migrated = self.journal.data["deliveries"]
        self.assertEqual(migrated[0], delivered)
        self.assertEqual([key for d in migrated[1:] for key in d["keys"]], keys)
        self.assertTrue(all(len(d["keys"]) <= MAX_RECEIPTS for d in migrated[1:]))

    def test_lock_prevents_second_adapter(self):
        with self.assertRaises(BlockingIOError):
            Journal(self.path, {"thread": "thread"})

    async def test_idle_starts_same_thread_without_policy_overrides(self):
        rpc = FakeRpc()
        self.assertTrue(await self.deliver(rpc))
        method, params = rpc.calls[-1]
        self.assertEqual(method, "turn/start")
        self.assertEqual(set(params), {"threadId", "input", "clientUserMessageId"})
        self.assertEqual(params["threadId"], "thread")
        self.assertEqual(self.delivery["status"], "delivered")

    async def test_active_steers_expected_turn_only(self):
        rpc = FakeRpc("active")
        await self.deliver(rpc)
        method, params = rpc.calls[-1]
        self.assertEqual(method, "turn/steer")
        self.assertEqual(params["expectedTurnId"], "turn")

    async def test_approval_and_input_waits_remain_pending(self):
        for flag in ("waitingOnApproval", "waitingOnUserInput"):
            rpc = FakeRpc("active", [flag])
            self.assertFalse(await self.deliver(rpc))
            self.assertEqual(len(rpc.calls), 1)
            self.assertEqual(self.delivery["status"], "pending")

    async def test_explicit_turn_race_can_reread_then_start(self):
        rpc = FakeRpc("active", failure=Rejected("turn changed"))
        with self.assertRaises(Rejected):
            await self.deliver(rpc)
        self.assertEqual(self.delivery["status"], "pending")
        rpc.status, rpc.failure = {"type": "idle"}, None
        await self.deliver(rpc)
        self.assertEqual(rpc.calls[-1][0], "turn/start")

    async def test_turn_completed_between_reads_sends_nothing(self):
        rpc = FakeRpc("active", turn_status="completed")
        with self.assertRaises(Rejected):
            await self.deliver(rpc)
        self.assertFalse(any(m.startswith("turn/") for m, _ in rpc.calls))

    async def test_lost_response_blocks_restart_not_duplicate_delivery(self):
        with self.assertRaises(ConnectionError):
            await self.deliver(FakeRpc(failure=ConnectionError("lost response")))
        self.assertEqual(json.loads(self.path.read_text())["deliveries"][0]["status"], "sending")
        self.journal.lock.close()
        with self.assertRaisesRegex(ValueError, "uncertain delivery"):
            Journal(self.path, {"thread": "thread"})

    async def test_success_survives_restart_and_new_batch(self):
        await self.deliver(FakeRpc())
        self.journal.lock.close()
        self.journal = Journal(self.path, {"thread": "thread"})
        self.assertFalse(self.journal.add(packet(batch="e" * 32), "manager"))
        self.assertIsNone(self.journal.pending())

    async def test_wrong_workspace_cannot_mutate_target(self):
        self.config.cwd = "/does-not-exist"
        rpc = FakeRpc()
        with self.assertRaisesRegex(ValueError, "mismatch"):
            await target(rpc, self.config)
        self.assertEqual(len(rpc.calls), 1)

    async def test_unloaded_thread_never_created_or_resumed(self):
        rpc = FakeRpc("notLoaded")
        with self.assertRaisesRegex(ValueError, "already be loaded"):
            await self.deliver(rpc)
        self.assertEqual(len(rpc.calls), 1)

    async def test_disconnect_during_host_reads_prevents_start_and_steer(self):
        for state in ("idle", "active"):
            rpc = FakeRpc(state)
            self.assertFalse(await self.deliver(rpc, lambda: not rpc.calls))
            self.assertFalse(any(method.startswith("turn/") for method, _ in rpc.calls))
            self.assertEqual(self.delivery["status"], "pending")

    async def test_wake_uses_configured_binary_and_session_for_receipt_commands(self):
        self.config.fray = "/path with spaces/fray"
        rpc = FakeRpc()
        await self.deliver(rpc)
        prompt = rpc.calls[-1][1]["input"][0]["text"]
        self.assertIn("'/path with spaces/fray'", prompt)
        self.assertIn("--session session", prompt)
        self.assertIn("ack --receipts JSON", prompt)

    async def test_real_unix_websocket_routes_responses_ignores_approval(self):
        socket = str(Path(self.temp.name) / "rpc.sock")
        seen = []
        async def server(ws):
            request = json.loads(await ws.recv())
            seen.append(request)
            await ws.send(json.dumps({"id": 900, "method": "item/commandExecution/requestApproval", "params": {}}))
            await ws.send(json.dumps({"method": "thread/status/changed", "params": {}}))
            await ws.send(json.dumps({"id": request["id"], "result": {"ok": True}}))
            async for message in ws:
                seen.append(json.loads(message))
        changed = asyncio.Event()
        async with unix_serve(server, socket):
            async with unix_connect(socket) as ws:
                rpc = Rpc(ws, changed)
                self.assertEqual(await rpc.call("thread/read", {"threadId": "thread"}), {"ok": True})
                self.assertTrue(changed.is_set())
                await rpc.close()
        self.assertEqual(len(seen), 1)

    async def test_controller_reconnects_defers_approval_and_reaps_watcher(self):
        socket = str(Path(self.temp.name) / "controller.sock")
        pidfile = Path(self.temp.name) / "watch.pid"
        executable = Path(self.temp.name) / "fake-fray"
        executable.write_text(f"#!{sys.executable}\nimport os,time\n"
                              f"open({str(pidfile)!r}, 'w').write(str(os.getpid()))\n"
                              f"print({json.dumps({'type': 'ready', 'control_version': 1, 'agent': 'manager', 'store_id': 'a' * 32})!r}, flush=True)\n"
                              f"print({json.dumps(packet())!r}, flush=True)\ntime.sleep(60)\n")
        executable.chmod(0o700)
        config = SimpleNamespace(**{**vars(self.config), "socket": socket, "fray": str(executable),
                                    "selection": "all", "lifetime": 15, "max_deliveries": 1, "probe": False})
        connections, calls = [], []
        waiting = True
        async def server(ws):
            nonlocal waiting
            connections.append(ws)
            async for raw in ws:
                request = json.loads(raw)
                if "id" not in request:
                    continue
                method, params = request["method"], request["params"]
                calls.append((method, params))
                if len(connections) == 1:
                    await ws.close()  # Before mutation: reconnect is safe.
                    return
                if method == "thread/read":
                    status = ({"type": "active", "activeFlags": ["waitingOnApproval"]}
                              if waiting else {"type": "idle"})
                    result = {"thread": {"id": "thread", "cwd": "/tmp", "status": status}}
                elif method == "turn/start":
                    self.assertFalse(waiting)
                    result = {"turn": {"id": "woken-turn"}}
                else:
                    result = {}
                await ws.send(json.dumps({"id": request["id"], "result": result}))
                if method == "thread/resume":
                    async def release():
                        nonlocal waiting
                        await asyncio.sleep(0.7)
                        waiting = False
                        await ws.send(json.dumps({"method": "thread/status/changed", "params": {}}))
                    release_task = asyncio.create_task(release())
        async with unix_serve(server, socket):
            await asyncio.wait_for(run(config, self.journal), 10)
        self.assertEqual(len(connections), 2)
        self.assertEqual([m for m, _ in calls].count("turn/start"), 1)
        self.assertEqual(self.delivery["turn_id"], "woken-turn")
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pidfile.read_text()), 0)


if __name__ == "__main__":
    unittest.main()
