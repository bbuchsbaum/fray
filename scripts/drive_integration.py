"""Drive lifecycle tests: owned process groups, running-turn urgency, pointers.

Run: python3 -W error::ResourceWarning scripts/drive_integration.py target/debug/fray
"""

import json
import os
import pathlib
import signal
import subprocess
import sys
import textwrap
import time
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import integration as base  # noqa: E402

BINARY, AGENT = base.BINARY, base.AGENT

# Starts an in-group grandchild (and optionally a deliberately detached job),
# records their PIDs, then sleeps or exits.
WRAPPER = textwrap.dedent(
    """
    import json, os, signal, subprocess, sys, time
    if '--ignore-term' in sys.argv:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    sleeper = [sys.executable, '-c', 'import time; time.sleep(120)']
    grandchild = subprocess.Popen(sleeper)
    detached = subprocess.Popen(sleeper, start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) if '--detached' in sys.argv else None
    pids = {'wrapper': os.getpid(), 'grandchild': grandchild.pid,
            'detached': detached and detached.pid}
    tmp = sys.argv[1] + '.tmp'
    with open(tmp, 'w') as out:
        json.dump(pids, out)
    os.rename(tmp, sys.argv[1])
    if '--exit' in sys.argv:
        sys.exit(0)
    time.sleep(120)
"""
)

# First turn blocks (as a long model turn would); later turns ack and record.
TURNS = textwrap.dedent(
    """
    import json, os, pathlib, socket, sys, time
    prompt = sys.stdin.read()
    state = json.loads(prompt.split('CURRENT PROJECT STATE\\n', 1)[1])
    record = pathlib.Path(sys.argv[1])
    first = not record.exists()
    with record.open('a') as out:
        out.write(json.dumps(state) + '\\n')
    if first:
        time.sleep(60)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
        peer.connect(os.environ['FRAY_HOME'] + '/bus.sock')
        with peer.makefile('rwb', buffering=0) as stream:
            for item in state['attention']['items']:
                request = {'op': 'ack', 'actor': state['agent'],
                           'args': {'receipts': [item['receipt']]}}
                stream.write((json.dumps(request) + '\\n').encode())
                assert json.loads(stream.readline())['ok']
"""
)


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class Drive(unittest.TestCase):
    setUp = base.Integration.setUp
    launch = base.Integration.launch
    tearDown = base.Integration.tearDown
    call = base.Integration.call
    cli = base.Integration.cli
    controller = base.Integration.controller
    await_controller = base.Integration.await_controller

    def script(self, name, text):
        path = pathlib.Path(self.home) / name
        path.write_text(text)
        return str(path)

    def drive(self, *options, child, **kwargs):
        return [
            str(BINARY),
            "--home",
            self.home,
            "--as",
            "bob",
            "drive",
            *options,
            "--",
            sys.executable,
            *child,
        ]

    def pids(self, path, timeout=5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if path.exists():
                return json.loads(path.read_text())
            time.sleep(0.02)
        self.fail("wrapper did not record its PIDs")

    def assert_gone(self, *pids, timeout=3):
        deadline = time.monotonic() + timeout
        while any(alive(p) for p in pids) and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertEqual([p for p in pids if alive(p)], [], "owned processes survived")

    def keep(self, process):
        self.addCleanup(
            lambda: process.poll() is None and (process.kill(), process.wait())
        )
        return process

    def kill_later(self, pid):
        def stop():
            if pid and alive(pid):
                os.kill(pid, signal.SIGKILL)

        self.addCleanup(stop)

    def test_timeout_stops_wrapper_and_grandchild_but_not_detached_or_unrelated(self):
        sibling = self.keep(subprocess.Popen(["sleep", "60"]))
        wrapper = self.script("wrapper.py", WRAPPER)
        record = pathlib.Path(self.home) / "pids.json"
        started = time.monotonic()
        result = subprocess.run(
            self.drive(
                "--bootstrap",
                "--max-turns",
                "1",
                "--idle-timeout",
                "0",
                "--child-timeout",
                "1",
                child=[wrapper, str(record), "--detached"],
            ),
            text=True,
            capture_output=True,
            timeout=20,
        )
        pids = self.pids(record)
        self.kill_later(pids["detached"])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("child_timeout", result.stderr)
        self.assertLess(time.monotonic() - started, 10)
        self.assert_gone(pids["wrapper"], pids["grandchild"])
        self.assertTrue(
            alive(pids["detached"]), "a job that left the group is not owned"
        )
        self.assertIsNone(sibling.poll(), "an unrelated process was signalled")
        controller = self.controller("bob")
        self.assertEqual(
            (controller["state"], controller["reason"]), ("failed", "child_timeout")
        )
        child = controller["detail"]["child"]
        self.assertEqual(child["pgid"], pids["wrapper"])
        self.assertEqual(child["disposition"]["signals"], ["TERM"])
        self.assertTrue(child["disposition"]["verified"])

    def test_group_ignoring_term_is_killed_and_verified(self):
        wrapper = self.script("wrapper.py", WRAPPER)
        record = pathlib.Path(self.home) / "pids.json"
        result = subprocess.run(
            self.drive(
                "--bootstrap",
                "--max-turns",
                "1",
                "--idle-timeout",
                "0",
                "--child-timeout",
                "1",
                child=[wrapper, str(record), "--ignore-term"],
            ),
            text=True,
            capture_output=True,
            timeout=20,
        )
        pids = self.pids(record)
        self.assertIn("child_timeout", result.stderr)
        self.assert_gone(pids["wrapper"], pids["grandchild"])
        disposition = self.controller("bob")["detail"]["child"]["disposition"]
        self.assertEqual(disposition["signals"], ["TERM", "KILL"])
        self.assertTrue(disposition["verified"])

    def test_successful_turn_does_not_leave_a_background_writer(self):
        wrapper = self.script("wrapper.py", WRAPPER)
        record = pathlib.Path(self.home) / "pids.json"
        result = subprocess.run(
            self.drive(
                "--bootstrap",
                "--max-turns",
                "1",
                "--idle-timeout",
                "0",
                child=[wrapper, str(record), "--exit"],
            ),
            text=True,
            capture_output=True,
            timeout=20,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        pids = self.pids(record)
        self.assert_gone(pids["grandchild"])
        controller = self.controller("bob")
        self.assertEqual(controller["state"], "stopped")
        self.assertTrue(controller["detail"]["child"]["disposition"]["verified"])

    def test_runner_death_or_interrupt_still_stops_its_group(self):
        wrapper = self.script("wrapper.py", WRAPPER)
        for sig in (signal.SIGKILL, signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=sig):
                record = pathlib.Path(self.home) / f"pids-{sig}.json"
                runner = self.keep(
                    subprocess.Popen(
                        self.drive(
                            "--bootstrap",
                            "--max-turns",
                            "1",
                            "--child-timeout",
                            "60",
                            child=[wrapper, str(record)],
                        ),
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                    )
                )
                pids = self.pids(record)
                runner.send_signal(sig)
                runner.wait(timeout=5)
                self.assert_gone(pids["wrapper"], pids["grandchild"], timeout=8)
                # The runner never claimed a clean stop for work it did not dispose of.
                self.assertNotEqual(self.controller("bob")["last_state"], "stopped")
                # Release the identity for the next case: the lease is still live.
                self.call("leave", "bob")
                self.call("join", "bob")

    def test_restart_refuses_while_a_previous_owned_group_lives(self):
        orphan = self.keep(subprocess.Popen(["sleep", "60"], process_group=0))
        self.call(
            "controller", "bob", {"run_id": "old", "state": "waiting", "begin": True}
        )
        self.call(
            "controller",
            "bob",
            {
                "run_id": "old",
                "state": "failed",
                "reason": "crashed",
                "detail": {
                    "child": {
                        "pid": orphan.pid,
                        "pgid": orphan.pid,
                        "disposition": None,
                    }
                },
            },
        )
        refused = subprocess.run(
            self.drive("--idle-timeout", "0", child=["-c", "pass"]),
            text=True,
            capture_output=True,
            timeout=10,
        )
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("orphaned_child", refused.stderr)
        self.assertIn(str(orphan.pid), refused.stderr)
        self.assertEqual(self.controller("bob")["run_id"], "old")
        self.assertIsNone(orphan.poll(), "the guard must not signal the group itself")
        wrong = subprocess.run(
            self.drive(
                "--idle-timeout",
                "0",
                "--release-orphan",
                str(orphan.pid + 1),
                child=["-c", "pass"],
            ),
            text=True,
            capture_output=True,
            timeout=10,
        )
        self.assertIn("orphaned_child", wrong.stderr)
        released = subprocess.run(
            self.drive(
                "--idle-timeout",
                "0",
                "--release-orphan",
                str(orphan.pid),
                child=["-c", "pass"],
            ),
            text=True,
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(released.returncode, 0, released.stderr)
        self.assertIn("released_orphan", released.stderr)
        orphan.kill()
        orphan.wait()
        # An empty group needs no release.
        again = subprocess.run(
            self.drive("--idle-timeout", "0", child=["-c", "pass"]),
            text=True,
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(again.returncode, 0, again.stderr)

    def objection_for_bob(self):
        card = self.call(
            "post", "bob", {"kind": "task", "title": "Candidate", "summary": "Ready"}
        )
        result = self.call(
            "annotate",
            "alice",
            {
                "id": card["card"]["id"],
                "kind": "objection",
                "body": "Stop: rejected production hash.",
            },
        )
        follow_up = result.get("follow_up_id") or result.get("follow_up", {}).get("id")
        return card["card"]["id"], follow_up

    def queued(self):
        detail = self.controller("bob").get("detail") or {}
        return detail.get("queued_urgent") or []

    def await_queued(self, timeout=6):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if queued := self.queued():
                return queued
            time.sleep(0.05)
        self.fail("urgent attention was never reported as queued")

    def test_objection_during_running_turn_is_reported_queued_not_presented(self):
        self.cli("alice", "send", "bob", "Please run the trial.", "--ask")
        record = pathlib.Path(self.home) / "turns.jsonl"
        runner = self.keep(
            subprocess.Popen(
                self.drive(
                    "--max-turns",
                    "1",
                    "--child-timeout",
                    "8",
                    child=[self.script("turns.py", TURNS), str(record)],
                ),
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        )
        self.await_controller("bob", "running")
        presented = self.controller("bob")["detail"]["presented"]
        self.assertEqual(len(presented), 1)
        self.objection_for_bob()
        queued = self.await_queued()
        self.assertEqual(queued[0]["status"], "queued_not_presented")
        self.assertEqual(queued[0]["next_boundary"], "next managed turn")
        self.assertLessEqual(queued[0]["priority"], 1)
        pending = self.call("inbox", "bob")["total"]
        # Queue mode never interrupts: the turn runs to its own limit.
        self.assertEqual(self.controller("bob")["state"], "running")
        _, stderr = runner.communicate(timeout=20)
        self.assertIn("child_timeout", stderr)
        # Nothing was acknowledged on the child's behalf.
        self.assertEqual(self.call("inbox", "bob")["total"], pending)

        def delivery(card_id):
            shown = self.call("show", "bob", {"id": card_id, "receipts": True})
            return next(r for r in shown["receipts"] if r["agent"] == "bob")

        # Per recipient: what was handed over is exposed, not acknowledged;
        # the queued objection was neither.
        handed = delivery(presented[0]["id"])
        self.assertEqual(handed["exposed_seq"], presented[0]["through_seq"])
        self.assertEqual(handed["ack_seq"], 0)
        objection = delivery(queued[0]["id"])
        self.assertEqual((objection["exposed_seq"], objection["ack_seq"]), (0, 0))

    def test_interrupt_mode_restarts_the_turn_with_the_objection(self):
        self.cli("alice", "send", "bob", "Please run the trial.", "--ask")
        record = pathlib.Path(self.home) / "turns.jsonl"
        started = time.monotonic()
        runner = self.keep(
            subprocess.Popen(
                self.drive(
                    "--max-turns",
                    "2",
                    "--child-timeout",
                    "60",
                    "--on-urgent",
                    "interrupt",
                    child=[self.script("turns.py", TURNS), str(record)],
                ),
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        )
        self.await_controller("bob", "running")
        # Ordinary chatter does not interrupt a running turn.
        self.cli("alice", "send", "bob", "FYI: lunch is at noon.")
        time.sleep(3)
        self.assertEqual(len(record.read_text().splitlines()), 1)
        card_id, _ = self.objection_for_bob()
        _, stderr = runner.communicate(timeout=30)
        self.assertEqual(runner.returncode, 0, stderr)
        self.assertLess(time.monotonic() - started, 25)
        turns = [json.loads(line) for line in record.read_text().splitlines()]
        self.assertEqual(len(turns), 2)
        metrics = [
            json.loads(line.removeprefix("fray drive: "))
            for line in stderr.splitlines()
            if line.startswith("fray drive: ")
        ]
        self.assertEqual(metrics[0]["exit_reason"], "preempted")
        self.assertTrue(metrics[0]["child"]["disposition"]["verified"])
        second = turns[1]["attention"]["items"]
        self.assertLessEqual(
            second[0]["card"]["priority"], 1, "urgent attention comes first"
        )
        titles = json.dumps(second)
        self.assertIn("rejected production hash", titles)
        self.assertEqual(self.call("inbox", "bob")["total"], 0)
        del card_id

    def test_oversized_receipt_becomes_fetchable_pointer_beside_other_attention(self):
        big = self.cli("alice", "send", "bob", "🧠" * 300, "--ask")
        for _ in range(2):
            self.cli("alice", "reply", str(big["card"]["id"]), "🧠" * 300)
        small = self.cli("alice", "send", "bob", "Small independent question", "--ask")
        record = pathlib.Path(self.home) / "turns.jsonl"
        run = subprocess.run(
            self.drive(
                "--budget",
                "2000",
                "--max-turns",
                "1",
                child=[str(AGENT), str(record), "--ignore"],
            ),
            text=True,
            capture_output=True,
            timeout=10,
        )
        # An ignored pointer is still an unhandled receipt: nothing skipped or acked.
        self.assertIn("stalled", run.stderr)
        self.assertEqual(self.call("inbox", "bob")["total"], 2)
        turn = json.loads(record.read_text())
        self.assertLessEqual(turn["prompt_bytes"], 2000)
        items = {item["card"]["id"]: item for item in turn["attention"]["items"]}
        self.assertEqual(set(items), {big["card"]["id"], small["card"]["id"]})
        pointer = items[big["card"]["id"]]
        self.assertTrue(pointer["omitted"])
        self.assertEqual(pointer["fetch"], f"fray thread {big['card']['id']} --unread")
        self.assertIn("rev", pointer["card"])
        self.assertNotIn("omitted", items[small["card"]["id"]])
        # The omitted content is fetchable in full, with a receipt for what was read.
        thread = self.cli("bob", "thread", str(big["card"]["id"]), "--unread")
        self.assertIn("🧠" * 300, json.dumps(thread, ensure_ascii=False))
        self.assertEqual(
            thread["receipt"]["through_seq"], pointer["receipt"]["through_seq"]
        )
        # A new reply after presentation stays visible as newer than the pointer.
        self.cli("alice", "reply", str(big["card"]["id"]), "One more thing")
        inbox = self.call("inbox", "bob")
        newer = [i for i in inbox["items"] if i["card"]["id"] == big["card"]["id"]][0]
        self.assertGreater(newer["through_seq"], pointer["through_seq"])


if __name__ == "__main__":
    unittest.main(argv=sys.argv[:1], verbosity=2)
