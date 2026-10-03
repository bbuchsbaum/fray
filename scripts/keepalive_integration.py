"""Keepalive (docs/design/keepalive.md, K1) end to end, with stub hosts.

Scripts named `claude` and `codex` first on the daemon's PATH stand in for the
hosts: they record their arguments, read the packet, print fork JSON and usage
and emit structured output. No model is called.

Run: python3 -W error::ResourceWarning scripts/keepalive_integration.py target/debug/fray
(drive_integration.py imports these tests, so its gate runs them too.)
"""

import json
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import integration as base  # noqa: E402

BINARY = base.BINARY

# One stub for both hosts; it tells them apart by its own name. The `mode`
# file in its directory picks a behaviour: answer (default), outside (also
# names a card outside the packet), invalid (output failing the schema),
# slow:SECONDS (a long turn). Like codex 0.160, the codex stub reports its
# thread's running input total, and a fork's first total includes the
# parent's (the `parent` file).
STUB = r"""#!/usr/bin/env python3
import json, os, pathlib, sys, time, uuid
host = pathlib.Path(sys.argv[0]).name
argv = sys.argv[1:]
d = pathlib.Path(os.environ['KEEPALIVE_STUB'])
prompt = sys.stdin.read()
state = json.loads(prompt.split('CURRENT PROJECT STATE\n', 1)[1])
cards = [i['card']['id'] for i in state['attention']['items']]
mode = (d / 'mode').read_text().strip() if (d / 'mode').exists() else 'answer'
tokens = int((d / 'tokens').read_text()) if (d / 'tokens').exists() else 10
with (d / 'calls.jsonl').open('a') as out:
    out.write(json.dumps({'host': host, 'argv': argv, 'cards': cards, 'cwd': os.getcwd(),
                          'env': {k: os.environ.get(k) for k in ('FRAY_AGENT', 'FRAY_SESSION', 'FRAY_HOME', 'CLAUDECODE', 'CODEX_SANDBOX')}}) + '\n')
if mode.startswith('slow:'):
    time.sleep(float(mode.split(':')[1]))
decision = {'actions': [{'card': c, 'kind': 'answer', 'body': f'{host} keepalive answers #{c}'} for c in cards],
            'handled': cards, 'summary': f'answered {len(cards)}'}
if mode == 'outside':
    decision['actions'].append({'card': 999999, 'kind': 'note', 'body': 'not in my packet'})
if host == 'claude':
    resumed = argv[argv.index('--resume') + 1]
    session = argv[argv.index('--session-id') + 1] if '--fork-session' in argv else resumed
    result = {'type': 'result', 'subtype': 'success', 'is_error': False, 'session_id': session,
              'usage': {'input_tokens': tokens, 'cache_read_input_tokens': 0, 'cache_creation_input_tokens': 0}}
    result['structured_output'] = {'actions': 'all of them'} if mode == 'invalid' else decision
    print(json.dumps(result))
else:
    thread = 'fork-' + uuid.uuid4().hex[:12] if argv[1] == 'fork' else argv[-2]
    print(json.dumps({'type': 'thread.started', 'thread_id': thread}), flush=True)
    text = 'not json at all' if mode == 'invalid' else json.dumps(decision)
    pathlib.Path(argv[argv.index('-o') + 1]).write_text(text)
    print(json.dumps({'type': 'item.completed', 'item': {'type': 'agent_message', 'text': text}}))
    totals = json.loads((d / 'totals.json').read_text()) if (d / 'totals.json').exists() else {}
    parent = int((d / 'parent').read_text()) if (d / 'parent').exists() else 0
    totals[thread] = totals.get(thread, parent) + tokens
    (d / 'totals.json').write_text(json.dumps(totals))
    print(json.dumps({'type': 'turn.completed', 'usage': {'input_tokens': totals[thread], 'cached_input_tokens': 0, 'output_tokens': 5}}))
"""

PINNED_CLAUDE = [
    "--tools",
    "Read,Grep,Glob",
    "--restricted",
    "--strict-mcp-config",
    "--permission-mode",
    "dontAsk",
    "--json-schema",
]
CODEX_FEATURES = [
    "multi_agent",
    "apps",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "computer_use",
    "image_generation",
    "plugins",
    "remote_plugin",
    "in_app_browser",
    "in_app_local_automation",
    "skill_mcp_dependency_install",
]
LIVE = ("starting", "keepalive", "paused", "deferred", "stopping")


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class Keepalive(unittest.TestCase):
    def setUp(self):
        # Short paths: the socket lives under the repository's .fray.
        self.root = pathlib.Path(tempfile.mkdtemp(prefix="fk-", dir="/tmp"))
        self.repo = self.root / "repo"
        self.repo.mkdir()
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        self.home = str(self.repo / ".fray")
        self.stub = self.root / "stub"
        self.stub.mkdir()
        for host in ("claude", "codex"):
            path = self.stub / host
            path.write_text(STUB)
            path.chmod(0o755)
        env = {
            k: v
            for k, v in os.environ.items()
            if k
            not in (
                "FRAY_AGENT",
                "FRAY_SESSION",
                "FRAY_HOME",
                "CLAUDE_CODE_SESSION_ID",
                "CODEX_THREAD_ID",
                "CODEX_SANDBOX",
            )
        }
        self.env = dict(
            env,
            PATH=f"{self.stub}:{os.environ['PATH']}",
            KEEPALIVE_STUB=str(self.stub),
            FRAY_TEST_SANDBOXED="0",
        )
        self.server = None
        self.log = open(self.root / "server.log", "ab")
        self.launch()
        self.fray("join", actor="bob", session="test:bob")

    def launch(self, **extra):
        # As if started from inside hosts: a turn must not inherit either.
        self.server = subprocess.Popen(
            [str(BINARY), "--home", self.home, "serve"],
            env=dict(self.env, CLAUDECODE="1", CODEX_SANDBOX="seatbelt", **extra),
            stdout=self.log,
            stderr=self.log,
        )
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            try:
                if self.rpc("ping").get("ok"):
                    return
            except OSError:
                pass
            time.sleep(0.02)
        self.fail("daemon did not become ready")

    def shutdown(self):
        if self.server and self.server.poll() is None:
            pids = [s["pid"] for s in self.statuses() if s.get("pid")]
            try:
                self.rpc("shutdown")
            except OSError:
                pass
            self.server.wait(timeout=5)
            # A drive whose daemon has gone fails its next call and exits.
            deadline = time.monotonic() + 10
            while any(alive(p) for p in pids) and time.monotonic() < deadline:
                time.sleep(0.05)
            for pid in pids:
                if alive(pid):
                    os.kill(pid, signal.SIGKILL)

    def tearDown(self):
        self.shutdown()
        self.log.close()
        shutil.rmtree(self.root, ignore_errors=True)

    def statuses(self):
        try:
            agents = self.rpc("agents")["data"]["items"]
        except (OSError, KeyError):
            return []
        return [
            self.rpc("keepalive_status", "bob", {"agent": a["name"]})["data"]
            for a in agents
        ]

    def rpc(self, op, actor="", args=None, session=None):
        request = {"op": op, "actor": actor, "args": args or {}}
        if session:
            request["session"] = session
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
            peer.settimeout(10)
            peer.connect(self.home + "/bus.sock")
            with peer.makefile("rwb", buffering=0) as stream:
                stream.write((json.dumps(request) + "\n").encode())
                return json.loads(stream.readline())

    def fray(self, *args, actor, session=None, cwd=None, ok=True):
        env = dict(self.env)
        if session:
            env["FRAY_SESSION"] = session
        result = subprocess.run(
            [str(BINARY), "--home", self.home, "--as", actor, "--json", *args],
            env=env,
            cwd=cwd or self.repo,
            text=True,
            capture_output=True,
            timeout=15,
        )
        value = json.loads(result.stdout) if result.stdout.strip() else {}
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return value

    def status(self, actor):
        return self.fray("keepalive", "--status", actor=actor)

    def await_status(self, actor, check, what, timeout=10):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            status = self.status(actor)
            if check(status):
                return status
            time.sleep(0.05)
        self.fail(f"{actor} keepalive never {what}: {status}\n" + self.drive_log(actor))

    def drive_log(self, actor):
        path = pathlib.Path(self.home) / "keepalive" / f"{actor}.log"
        return path.read_text() if path.exists() else "(no log)"

    def start(self, actor, session, cwd=None):
        self.fray("join", actor=actor, session=session)
        started = self.fray("keepalive", actor=actor, session=session, cwd=cwd)
        self.assertTrue(started["started"], started)
        self.await_status(actor, lambda s: s["state"] == "keepalive", "began waiting")
        return started

    def ask(self, actor, text):
        return self.fray("send", actor, text, "--ask", actor="bob", session="test:bob")[
            "card"
        ]["id"]

    def replies(self, card, actor):
        history = self.rpc("show", "bob", {"id": card, "history": True, "limit": 100})[
            "data"
        ]["history"]
        return [e for e in history if e["actor"] == actor and e["op"] == "annotate"]

    def await_reply(self, card, actor, timeout=10):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            replies = self.replies(card, actor)
            if replies:
                return replies
            time.sleep(0.05)
        self.fail(f"no reply from {actor} on #{card}\n" + self.drive_log(actor))

    def calls(self):
        path = self.stub / "calls.jsonl"
        return (
            [json.loads(line) for line in path.read_text().splitlines()]
            if path.exists()
            else []
        )

    def mode(self, mode):
        (self.stub / "mode").write_text(mode)

    def pending(self, actor):
        return self.rpc("inbox", actor)["data"]["total"]

    def test_claude_forks_then_resumes_answers_as_the_agent_and_survives_clear(self):
        started = self.start("alice", "claude:c0ffee-1")
        # The conversation is the bound session's; the command is fixed.
        self.assertEqual(started["command"], ["fray", "drive", "--keepalive"])
        self.assertEqual(
            (started["host"], started["companion"], started["session"]),
            ("claude", "claude:c0ffee-1", "keepalive:c0ffee-1"),
        )
        first = self.ask("alice", "Which parser did we pick?")
        reply = self.await_reply(first, "alice")[0]
        self.assertEqual(reply["payload"]["detail"]["kind"], "answer")
        self.assertIn("claude keepalive answers", reply["payload"]["detail"]["body"])
        call = self.calls()[0]
        fork = call["argv"][call["argv"].index("--session-id") + 1]
        self.assertEqual(
            call["argv"][:6],
            ["-p", "--resume", "c0ffee-1", "--fork-session", "--session-id", fork],
        )
        self.assertEqual(call["argv"][6:13], PINNED_CLAUDE)
        self.assertEqual(call["argv"][-2:], ["--output-format", "json"])
        self.assertEqual(
            json.loads(call["argv"][13])["required"], ["actions", "handled", "summary"]
        )
        # The turn gets no board identity, and runs in the terminal's directory.
        self.assertEqual(
            call["env"],
            dict.fromkeys(
                ("FRAY_AGENT", "FRAY_SESSION", "FRAY_HOME", "CLAUDECODE", "CODEX_SANDBOX")
            ),
        )
        self.assertEqual(pathlib.Path(call["cwd"]).resolve(), self.repo.resolve())
        status = self.await_status(
            "alice",
            lambda s: s["fork"] == fork and self.pending("alice") == 0,
            "recorded its fork",
        )
        self.assertEqual(status["usage"]["input_tokens"], 10)
        # The terminal's /clear rebinds the interactive side only.
        cleared = self.rpc(
            "join",
            "alice",
            {"takeover": True, "continued": "clear"},
            session="claude:c0ffee-2",
        )
        self.assertTrue(cleared["ok"], cleared)
        self.assertEqual(
            cleared["data"]["session_replaced"]["session"], "claude:c0ffee-1"
        )
        second = self.ask("alice", "And the lexer?")
        self.await_reply(second, "alice")
        later = self.calls()[1]
        self.assertEqual(later["argv"][:3], ["-p", "--resume", fork])
        self.assertNotIn("--fork-session", later["argv"])
        roster = next(
            a for a in self.rpc("agents")["data"]["items"] if a["name"] == "alice"
        )
        self.assertEqual(roster["session"]["bound"]["session"], "claude:c0ffee-2")
        self.assertEqual(roster["keepalive"]["state"], "keepalive")
        self.assertEqual(roster["reachability"], "wakeable")
        team = subprocess.run(
            [str(BINARY), "--home", self.home, "--as", "bob", "team"],
            env=self.env,
            cwd=self.repo,
            text=True,
            capture_output=True,
            timeout=15,
        ).stdout
        self.assertRegex(team, r"alice .* keepalive\n")
        # A stop request ends a waiting keepalive within seconds.
        asked = time.monotonic()
        self.assertEqual(
            self.fray("keepalive", "--stop", actor="alice", session="claude:c0ffee-2")[
                "state"
            ],
            "stopping",
        )
        self.await_status(
            "alice", lambda s: s["state"] == "stopped", "stopped", timeout=5
        )
        self.assertLess(time.monotonic() - asked, 3)
        self.assertEqual(self.status("alice")["reason"], "stopped")

    def test_codex_reads_its_fork_from_the_stream_and_refuses_cards_outside_its_packet(
        self,
    ):
        self.mode("outside")
        # The fork's first running total includes the parent's history.
        (self.stub / "parent").write_text("1000")
        self.start("carol", "codex:0193-ab")
        first = self.ask("carol", "Status of the build?")
        self.await_reply(first, "carol")
        call = self.calls()[0]
        argv = call["argv"]
        self.assertEqual(
            argv[:5], ["exec", "fork", "--json", "--output-schema", argv[4]]
        )
        self.assertEqual(
            json.loads(pathlib.Path(argv[4]).read_text())["additionalProperties"], False
        )
        self.assertEqual(argv[5], "-o")
        self.assertEqual(
            argv[7:17],
            [
                "--ignore-user-config",
                "--ignore-rules",
                "-c",
                "features.hooks=false",
                "-c",
                'approval_policy="never"',
                "-c",
                'sandbox_mode="read-only"',
                "-c",
                'web_search="disabled"',
            ],
        )
        self.assertEqual(
            argv[17:-2],
            [x for f in CODEX_FEATURES for x in ("-c", f"features.{f}=false")],
        )
        self.assertEqual(argv[-2:], ["0193-ab", "-"])
        status = self.await_status(
            "carol",
            lambda s: (s["fork"] or "").startswith("fork-"),
            "recorded its fork",
        )
        # The refused action is reported on the first packet card, and nothing
        # reached the card it named; the handled receipt was acknowledged.
        notes = [
            r["payload"]["detail"]["body"]
            for r in self.replies(first, "carol")
            if r["payload"]["detail"]["kind"] == "note"
        ]
        self.assertTrue(any("#999999" in n for n in notes), notes)
        self.assertIn('"refused":[999999]', self.drive_log("carol"))
        self.assertEqual(self.pending("carol"), 0)
        self.assertEqual(status["usage"]["input_tokens"], 1010)
        # Output failing the schema posts and acknowledges nothing; the
        # keepalive rests before trying again, and a stop ends the rest.
        self.mode("invalid")
        second = self.ask("carol", "And the tests?")
        failed = self.await_status(
            "carol", lambda s: s["failures"] == 1, "failed a turn"
        )
        later = self.calls()[1]["argv"]
        self.assertEqual(later[:2], ["exec", "resume"])
        self.assertEqual(later[-2:], [status["fork"], "-"])
        self.assertEqual(failed["state"], "keepalive")
        self.assertEqual(self.replies(second, "carol"), [])
        self.assertEqual(self.pending("carol"), 1)
        self.assertIn("final message is not JSON", self.drive_log("carol"))
        # The resumed thread reported 1020 in all: charged the rise, 10.
        self.assertEqual(failed["usage"]["input_tokens"], 1020)
        asked = time.monotonic()
        self.fray("keepalive", "--stop", actor="carol", session="codex:0193-ab")
        self.await_status(
            "carol", lambda s: s["state"] == "stopped", "stopped", timeout=5
        )
        self.assertLess(time.monotonic() - asked, 3)
        self.assertEqual(len(self.calls()), 2)

    def test_stop_lets_a_running_turn_finish_then_exits(self):
        self.mode("slow:2")
        session = "claude:0193a1b2-7c3d-7e4f-8a9b-0c1d2e3f4a5b"
        self.start("dave", session)
        card = self.ask("dave", "Long question")
        self.await_status("dave", lambda s: s["turn"] == "running", "ran a turn")
        self.assertEqual(
            self.fray("keepalive", "--stop", actor="dave", session=session)["state"],
            "stopping",
        )
        # Stopping takes no further turns, so it no longer counts as wakeable.
        roster = next(
            a for a in self.rpc("agents")["data"]["items"] if a["name"] == "dave"
        )
        self.assertNotEqual(roster["reachability"], "wakeable")
        self.assertEqual(roster["keepalive"]["companion"], "claude:0193a1b2-7c3d…")
        text = subprocess.run(
            [str(BINARY), "--home", self.home, "--as", "dave", "keepalive", "--status"],
            env=self.env,
            cwd=self.repo,
            text=True,
            capture_output=True,
            timeout=15,
        ).stdout
        self.assertIn("serves: claude:0193a1b2-7c3d… (claude)", text)
        team = subprocess.run(
            [str(BINARY), "--home", self.home, "--as", "bob", "team"],
            env=self.env,
            cwd=self.repo,
            text=True,
            capture_output=True,
            timeout=15,
        ).stdout
        self.assertIn("keepalive stopping", team)
        self.await_status(
            "dave", lambda s: s["state"] == "stopped", "stopped", timeout=10
        )
        # The turn in progress was applied before the drive exited.
        self.assertEqual(len(self.replies(card, "dave")), 1)
        self.assertEqual(len(self.calls()), 1)
        self.assertFalse(
            alive(self.status("dave")["pid"]) and self.status("dave")["pid_alive"]
        )

    def test_cards_too_large_for_the_packet_are_never_acked_or_paid_for(self):
        self.shutdown()
        self.launch(FRAY_TEST_KEEPALIVE_BUDGET="3000")
        self.fray("join", actor="ivy", session="claude:i1")
        big = self.ask("ivy", "Large: " + "x" * 2500)
        small = self.ask("ivy", "Small question")
        self.start("ivy", "claude:i1")
        self.await_reply(small, "ivy")
        # The turn saw the large card only as a pointer, and the stub listed it
        # in actions and handled anyway: refused, and its receipt not acked.
        first = self.calls()[0]
        self.assertEqual(sorted(first["cards"]), sorted([big, small]))
        self.assertEqual(self.replies(big, "ivy"), [])
        status = self.await_status(
            "ivy", lambda s: s["oversized"] == [big], "set the large card aside"
        )
        self.assertEqual((status["state"], status["failures"]), ("keepalive", 0))
        self.assertEqual(self.pending("ivy"), 1)
        # A packet of pointers alone costs no turn, and burns no failures.
        bigger = self.ask("ivy", "Larger: " + "y" * 2500)
        status = self.await_status(
            "ivy",
            lambda s: s["oversized"] == [big, bigger],
            "set the second large card aside",
        )
        time.sleep(0.5)
        self.assertEqual(len(self.calls()), 1)
        self.assertEqual((status["state"], status["failures"]), ("keepalive", 0))
        self.assertEqual(self.pending("ivy"), 2)
        # It keeps answering what it can read, without the set-aside cards.
        later = self.ask("ivy", "Another small one")
        self.await_reply(later, "ivy")
        self.assertEqual(self.calls()[1]["cards"], [later])
        self.assertEqual(self.pending("ivy"), 2)
        self.assertIn("keepalive_set_aside", self.drive_log("ivy"))
        text = subprocess.run(
            [str(BINARY), "--home", self.home, "--as", "ivy", "keepalive", "--status"],
            env=self.env,
            cwd=self.repo,
            text=True,
            capture_output=True,
            timeout=15,
        ).stdout
        self.assertIn(f"left unread (too large for a background turn): #{big}, #{bigger}", text)

    def test_budget_pauses_visibly_and_takes_no_turns(self):
        self.shutdown()
        self.launch(FRAY_KEEPALIVE_DAILY_TOKENS="100")
        (self.stub / "tokens").write_text("150")
        self.start("erin", "codex:e1-2")
        first = self.ask("erin", "One question")
        self.await_reply(first, "erin")
        status = self.await_status("erin", lambda s: s["state"] == "paused", "paused")
        self.assertEqual(status["paused"], "budget")
        self.assertEqual(
            status["usage"],
            {
                "day": status["usage"]["day"],
                "input_tokens": 150,
                "budget": 100,
                "over_budget": True,
            },
        )
        second = self.ask("erin", "Another question")
        time.sleep(1.5)
        self.assertEqual(self.replies(second, "erin"), [])
        self.assertEqual(len(self.calls()), 1)
        roster = next(
            a for a in self.rpc("agents")["data"]["items"] if a["name"] == "erin"
        )
        self.assertNotEqual(roster["reachability"], "wakeable")
        team = subprocess.run(
            [str(BINARY), "--home", self.home, "--as", "bob", "team"],
            env=self.env,
            cwd=self.repo,
            text=True,
            capture_output=True,
            timeout=15,
        ).stdout
        self.assertIn("keepalive paused (budget)", team)
        text = subprocess.run(
            [str(BINARY), "--home", self.home, "--as", "erin", "keepalive", "--status"],
            env=self.env,
            cwd=self.repo,
            text=True,
            capture_output=True,
            timeout=15,
        ).stdout
        self.assertIn("paused (budget)", text)
        self.fray("keepalive", "--stop", actor="erin", session="codex:e1-2")
        self.await_status(
            "erin", lambda s: s["state"] == "stopped", "stopped", timeout=5
        )

    def test_requests_are_refused_unless_bound_in_the_repository_and_unarmed(self):
        self.fray("join", actor="alice", session="claude:a1")
        refused = self.fray(
            "keepalive", actor="alice", session="claude:a1", cwd="/tmp", ok=False
        )
        self.assertEqual(refused["error"]["code"], "outside_repository")
        # A request may carry only the directory: never a program or arguments.
        extra = self.rpc(
            "keepalive_start",
            "alice",
            {"cwd": str(self.repo), "program": "sh"},
            session="claude:a1",
        )
        self.assertEqual(extra["error"]["code"], "invalid")
        # The conversation comes only from a bound Claude or Codex session.
        unbound = self.rpc("keepalive_start", "alice", {"cwd": str(self.repo)})
        self.assertEqual(unbound["error"]["code"], "session_required")
        self.fray("join", actor="frank", session="test:frank")
        other = self.fray("keepalive", actor="frank", session="test:frank", ok=False)
        self.assertEqual(other["error"]["code"], "keepalive_host")
        foreign = self.fray(
            "keepalive", actor="alice", session="claude:someone-else", ok=False
        )
        self.assertEqual(foreign["error"]["code"], "identity_busy")
        # A Claude with an armed Monitor is wakeable already.
        self.fray("join", actor="gina", session="claude:g1")
        monitor = subprocess.Popen(
            [
                str(BINARY),
                "--home",
                self.home,
                "--as",
                "gina",
                "watch",
                "--attention",
                "--notification",
                "--reconnect",
                "--activation",
                "native-monitor",
            ],
            env=dict(self.env, FRAY_SESSION="claude:g1"),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                roster = next(
                    a
                    for a in self.rpc("agents")["data"]["items"]
                    if a["name"] == "gina"
                )
                if roster["listener"] and roster["listener"]["live"]:
                    break
                time.sleep(0.05)
            armed = self.fray("keepalive", actor="gina", session="claude:g1", ok=False)
            self.assertEqual(armed["error"]["code"], "monitor_armed")
        finally:
            monitor.terminate()
            monitor.wait(timeout=5)
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.status("alice")["state"], "off")

    def test_a_git_worktree_of_the_repository_is_accepted(self):
        git = ["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@t"]
        subprocess.run(
            [*git, "commit", "-q", "--allow-empty", "-m", "init"], check=True
        )
        worktree = self.root / "wt"
        subprocess.run([*git, "worktree", "add", "-q", str(worktree)], check=True)
        started = self.start("hal", "claude:h1", cwd=worktree)
        self.assertEqual(pathlib.Path(started["cwd"]), worktree.resolve())
        self.fray("keepalive", "--stop", actor="hal", session="claude:h1")
        self.await_status(
            "hal", lambda s: s["state"] == "stopped", "stopped", timeout=5
        )

    def test_a_sandboxed_daemon_refuses(self):
        self.shutdown()
        self.launch(FRAY_TEST_SANDBOXED="1")
        self.fray("join", actor="alice", session="claude:a1")
        refused = self.fray("keepalive", actor="alice", session="claude:a1", ok=False)
        self.assertEqual(refused["error"]["code"], "sandboxed")
        self.assertIn("owner", refused["error"]["message"])
        self.assertEqual(self.status("alice")["state"], "off")


if __name__ == "__main__":
    unittest.main(argv=sys.argv[:1], verbosity=2)
