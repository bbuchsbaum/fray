#!/usr/bin/env python3
"""Black-box tests for a BUILT Fray binary; only Python's standard library needed.
Usage: python3 scripts/integration.py [target/debug/fray]
"""
import concurrent.futures, json, os, pathlib, shutil, socket, subprocess, sys, tempfile, threading, time, unittest

BINARY = pathlib.Path(sys.argv[1] if len(sys.argv)>1 else 'target/debug/fray').resolve()
AGENT = pathlib.Path(__file__).resolve().parents[1] / 'tests/fixtures/agent.py'
if len(sys.argv)>1: del sys.argv[1]
if not BINARY.is_file():
    sys.exit(f'Build first with cargo build; binary not found: {BINARY}')

class Peer:
    def __init__(self, home):
        self.socket=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
        self.socket.settimeout(8)
        try:
            self.socket.connect(str(pathlib.Path(home)/'bus.sock'))
        except OSError:
            self.socket.close()
            raise
        self.file=self.socket.makefile('rwb',buffering=0)
    def send(self,op,actor='',args=None,key=None):
        req={'op':op,'actor':actor,'args':args or {}}
        if key is not None: req['key']=key
        self.file.write((json.dumps(req)+'\n').encode())
    def receive(self):
        line=self.file.readline()
        if not line: raise EOFError('server closed connection')
        return json.loads(line)
    def call(self,op,actor='',args=None,key=None):
        self.send(op,actor,args,key)
        return self.receive()
    def close(self): self.file.close(); self.socket.close()
    def __enter__(self): return self
    def __exit__(self,*args): self.close()

class Integration(unittest.TestCase):
    def setUp(self):
        # Keep Unix-domain socket paths short even on macOS.
        self.home=tempfile.mkdtemp(prefix='fray-it-',dir='/tmp')
        self.log=open(pathlib.Path(self.home)/'test-server.log','ab')
        self.launch()
        self.call('join','alice');self.call('join','bob')
    def launch(self):
        self.server=subprocess.Popen([str(BINARY),'--home',self.home,'serve'],stdout=self.log,stderr=self.log)
        deadline=time.monotonic()+8
        while time.monotonic()<deadline:
            try:
                with Peer(self.home) as p:
                    if p.call('ping')['ok']: return
            except (OSError,EOFError): pass
            if self.server.poll() is not None: self.fail('daemon failed; see '+self.home)
            time.sleep(.015)
        self.fail('daemon did not become ready')
    def tearDown(self):
        if self.server.poll() is None:
            try: self.call('shutdown')
            except (OSError,EOFError): pass
            try: self.server.wait(timeout=3)
            except subprocess.TimeoutExpired: self.server.kill();self.server.wait()
        self.log.close();shutil.rmtree(self.home,ignore_errors=True)
    def call(self,op,actor='',args=None,key=None):
        with Peer(self.home) as p:
            result=p.call(op,actor,args,key)
        self.assertTrue(result['ok'],result)
        return result['data']
    def post(self,title='Work',**fields):
        return self.call('post','alice',{'kind':'task','title':title,'summary':'Implement and test',**fields})
    def cli(self, actor, *args, body=None):
        result = subprocess.run(
            [str(BINARY), '--home', self.home, '--as', actor, '--json', *args],
            input=body, text=True, capture_output=True, timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        return json.loads(result.stdout)
    def test_conversation_with_two_managers_and_scoped_peers_survives_restart(self):
        for actor, role, topics in [
            ('manager', 'steward', []), ('review-manager', 'steward', []),
            ('codex', 'worker', ['parser']), ('claude', 'reviewer', ['tests']),
            ('deepseek', 'worker', ['evidence']),
        ]:
            self.call('join', actor, {'role': role, 'topics': topics})
        question = self.cli('manager', '--key', 'review-one', 'send', 'codex', '-',
                            '--ask', '--ref', 'mote:parser-42', body='Is the parser ready for review?')
        id_ = str(question['card']['id'])
        self.assertEqual(self.call('inbox', 'codex')['total'], 1)
        self.assertEqual(self.call('inbox', 'review-manager')['total'], 1)
        self.assertEqual(self.call('inbox', 'claude')['total'], 0)
        self.assertEqual(self.call('inbox', 'deepseek')['total'], 0)
        self.cli('codex', 'reply', id_, 'Implementation ready; requesting an independent test review.')
        objection = self.cli('claude', 'reply', id_, 'Empty input lacks a regression test.', '--kind', 'objection')
        follow_up = objection['follow_up']['id']
        self.cli('deepseek', 'reply', id_, 'I reproduced the missing case.', '--kind', 'evidence')
        # A disabled participant must recover replies even outside its topic scope.
        self.call('leave', 'claude')
        final = self.cli('codex', 'reply', id_, 'Added the regression; the suite passes.')
        self.server.kill()
        self.server.wait()
        self.launch()
        self.assertEqual(self.call('ping')['store_id'], question['store_id'])
        self.assertEqual(self.call('ping')['protocol_version'], 2)
        rejoined = self.call('join', 'claude')
        receipt = next(x for x in rejoined['attention']['items'] if str(x['card']['id']) == id_)
        self.assertEqual(receipt['through_seq'], final['event_seq'])
        thread = self.cli('manager', 'thread', id_)
        self.assertEqual([x['actor'] for x in thread['history']], ['manager', 'codex', 'claude', 'deepseek', 'codex'])
        # Closing a parent is not acceptance of an unresolved objection: a plain
        # resolve is refused, and an explicit override is recorded while the
        # objection itself stays open.
        refused = subprocess.run([str(BINARY), '--home', self.home, '--as', 'manager', '--json', 'patch', id_,
                                  '--expect', str(thread['card']['rev']), '--status', 'resolved'],
                                 capture_output=True, text=True, timeout=10)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(json.loads(refused.stdout)['error']['code'], 'open_objections')
        self.cli('manager', 'patch', id_, '--expect', str(thread['card']['rev']), '--status', 'resolved',
                 '--over-objection', 'Manager closes the thread; the objection is handed to deepseek.')
        self.assertEqual(self.call('show', args={'id': follow_up})['card']['status'], 'open')
        overridden = self.call('show', args={'id': int(id_), 'history': True, 'limit': 100})['history'][-1]
        self.assertEqual(overridden['payload']['detail']['open_objections'], [follow_up])
        handoff = self.cli('manager', 'send', 'deepseek', 'Please verify the linked objection.', '--ask', '--ref', 'mote:parser-42')
        self.assertEqual(self.cli('deepseek', 'query', '--ref', 'mote:parser-42')['items'][0]['id'], handoff['card']['id'])
    def test_reply_refs_survive_restart_and_are_searchable(self):
        sent = self.cli('alice', 'send', 'bob', 'Review', '--ref', 'mote:original')
        id_ = str(sent['card']['id'])
        reply = self.cli('bob', '--key', 'reply-refs', 'reply', id_, 'Evidence',
                         '--ref', 'mote:new', '--ref', 'commit:abc', '--ref', 'mote:new')
        self.assertEqual(reply['card']['tags'], ['commit:abc', 'mote:new', 'mote:original'])
        self.assertEqual(self.cli('alice', 'query', '--ref', 'mote:new')['items'][0]['id'], int(id_))
        self.server.kill()
        self.server.wait()
        self.launch()
        thread = self.cli('alice', 'thread', id_)
        self.assertEqual(thread['card']['tags'], reply['card']['tags'])
        self.assertEqual(thread['history'][1]['payload']['detail']['refs'], ['commit:abc', 'mote:new'])

    def test_long_body_files_readable_threads_and_filtered_inbox(self):
        body = 'Contract\n' + 'λ' * 3900 + '\nend of contract\n'
        path = pathlib.Path(self.home) / 'contract.txt'
        path.write_text(body)
        sent = self.cli('alice', 'send', 'bob', '--body-file', str(path), '--ask')
        id_ = str(sent['card']['id'])
        reply = 'First line\n\tsecond line\nFinal finding'
        path.write_text(reply)
        replied = self.cli('bob', 'reply', id_, '--body-file', str(path), '--kind', 'evidence')
        self.server.kill(); self.server.wait(); self.launch()
        thread = self.cli('bob', 'thread', id_, '--bodies')
        self.assertEqual(thread['history'][0]['payload']['detail']['body'], body)
        self.assertEqual(thread['history'][1]['payload']['detail']['body'], reply)
        def plain(*args):
            result = subprocess.run([str(BINARY), '--home', self.home, '--as', 'bob',
                                     'thread', id_, '--bodies', *args],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            return result.stdout
        first = plain('--limit', '1')
        self.assertIn(body, first)
        self.assertIn(f"@{sent['event_seq']} alice question", first)
        self.assertIn(f"next --after {sent['event_seq']}", first)
        self.assertNotIn(reply, first)
        second = plain('--after', str(sent['event_seq']))
        self.assertIn(f"@{replied['event_seq']} bob evidence", second)
        self.assertIn(reply, second)
        self.assertNotIn('Contract\n', second)
        pending = self.cli('bob', 'inbox', '--addressed-to-me', '--unresolved')
        self.assertEqual(pending['total'], 1)
        self.cli('alice', 'patch', id_, '--expect', str(thread['card']['rev']), '--status', 'resolved')
        self.assertEqual(self.cli('bob', 'inbox', '--addressed-to-me', '--unresolved')['total'], 0)
        self.assertEqual(self.cli('bob', 'inbox')['total'], 1)
        path.write_text('x' * 8001)
        for args in [('send', 'bob', '--body-file', str(path)),
                     ('send', 'bob', 'inline', '--body-file', str(path)),
                     ('reply', id_, '--body-file', str(path))]:
            result = subprocess.run([str(BINARY), '--home', self.home, '--as', 'alice', *args],
                                    text=True, capture_output=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.cli('bob', 'thread', id_)['history']), 3)

    def test_find_discovers_boards_without_creating_or_switching_them(self):
        root = pathlib.Path(self.home) / 'workspace'
        root.mkdir()
        repo = root / 'repo'
        subprocess.run(['git', 'init', '-q', str(repo)], check=True, capture_output=True)
        git_home = repo / '.git' / 'fray'
        git_home.mkdir()
        shared = root / '.fray'
        shared.mkdir()
        other = root / 'other'
        other.mkdir()
        result = self.cli('alice', 'find', str(root))
        homes = {item['home']: item for item in result['items']}
        self.assertEqual(len(homes), 3)
        selected = str(pathlib.Path(self.home).resolve())
        self.assertEqual(result['selected_home'], selected)
        self.assertTrue(homes[selected]['running'])
        self.assertTrue(homes[selected]['selected'])
        self.assertFalse(homes[str(git_home.resolve())]['running'])
        self.assertFalse((other / '.fray').exists())
        self.assertEqual(list(shared.iterdir()), [])
        self.assertEqual(list(git_home.iterdir()), [])
        # Existing ancestor .fray is already the common home for sibling checkouts.
        env = {k:v for k,v in os.environ.items() if k not in ('FRAY_HOME', 'FRAY_AGENT')}
        default = subprocess.run([str(BINARY), '--json', 'find', '..'], cwd=repo, env=env,
                                 text=True, capture_output=True, timeout=10)
        self.assertEqual(default.returncode, 0, default.stderr)
        self.assertEqual(json.loads(default.stdout)['selected_home'], str(shared.resolve()))

    def test_drive_wakes_from_idle_and_supplies_new_attention(self):
        record = pathlib.Path(self.home) / 'turns.jsonl'
        runner = subprocess.Popen(
            [str(BINARY), '--home', self.home, '--as', 'bob', 'drive',
             '--max-turns', '1', '--idle-timeout', '5', '--', sys.executable, str(AGENT), str(record)],
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        try:
            self.await_controller('bob', 'waiting')
            self.assertFalse(record.exists(), 'empty startup invoked the agent')
            sent = self.cli('alice', 'send', 'bob', 'Please review the evidence.', '--ask')
            stdout, stderr = runner.communicate(timeout=10)
            self.assertEqual(runner.returncode, 0, stdout + stderr)
            turns = [json.loads(line) for line in record.read_text().splitlines()]
            self.assertEqual(len(turns), 1)
            self.assertEqual(turns[0]['attention']['items'][0]['through_seq'], sent['event_seq'])
            self.assertNotIn('available', turns[0])
            self.assertLessEqual(turns[0]['prompt_bytes'], 4000)
            self.assertEqual(self.call('inbox', 'bob')['total'], 0)
        finally:
            if runner.poll() is None:
                runner.kill()
            runner.communicate(timeout=5)
    def test_drive_stops_when_child_ignores_its_receipts(self):
        self.post(assignee='bob')
        record = pathlib.Path(self.home) / 'ignored.jsonl'
        result = subprocess.run(
            [str(BINARY), '--home', self.home, '--as', 'bob', 'drive',
             '--max-turns', '3', '--', sys.executable, str(AGENT), str(record), '--ignore'],
            text=True, capture_output=True, timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('stalled', result.stderr)
        self.assertEqual(len(record.read_text().splitlines()), 1)
        self.assertEqual(self.call('inbox', 'bob')['total'], 1)
    def controller(self, actor):
        return next(a for a in self.call('agents')['items'] if a['name'] == actor)['controller']
    def await_controller(self, actor, state):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            value = self.controller(actor)
            if value and value['state'] == state:
                return value
            time.sleep(.02)
        self.fail(f'{actor} did not become {state}')
    def run_agent(self, record, *options, agent_options=()):
        return subprocess.run(
            [str(BINARY), '--home', self.home, '--as', 'bob', 'drive',
             *options, '--', sys.executable, str(AGENT), str(record), *agent_options],
            text=True, capture_output=True, timeout=10)
    def test_empty_drive_does_not_invoke_child_but_explicit_bootstrap_does(self):
        record = pathlib.Path(self.home) / 'turns.jsonl'
        result = self.run_agent(record, '--idle-timeout', '0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(record.exists())
        self.assertEqual(self.controller('bob')['state'], 'stopped')
        result = self.run_agent(record, '--bootstrap', '--max-turns', '1')
        self.assertEqual(result.returncode, 0, result.stderr)
        turns = [json.loads(line) for line in record.read_text().splitlines()]
        self.assertEqual(len(turns), 1)
        self.assertEqual(turns[0]['attention']['total'], 0)
        self.assertLessEqual(turns[0]['prompt_bytes'], 4000)
    def test_unpresented_backlog_cannot_hide_an_ignored_packet(self):
        for i in range(12):
            self.cli('alice', 'send', 'bob', f'Question {i}', '--ask')
        record = pathlib.Path(self.home) / 'turns.jsonl'
        result = self.run_agent(record, '--max-turns', '3', agent_options=['--ignore'])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('stalled', result.stderr)
        turns = [json.loads(line) for line in record.read_text().splitlines()]
        self.assertEqual(len(turns), 1)
        self.assertLess(len(turns[0]['attention']['items']), 12)
        self.assertLessEqual(turns[0]['prompt_bytes'], 4000)
        self.assertEqual(self.call('inbox', 'bob')['total'], 12)
        self.assertEqual(self.controller('bob')['state'], 'failed')
        self.assertEqual(self.controller('bob')['reason'], 'stalled')
    def test_partial_progress_drains_backlog_without_requiring_one_huge_turn(self):
        for i in range(4):
            self.cli('alice', 'send', 'bob', f'Question {i}', '--ask')
        record = pathlib.Path(self.home) / 'turns.jsonl'
        result = self.run_agent(record, '--max-turns', '4', agent_options=['--partial'])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(record.read_text().splitlines()), 4)
        self.assertEqual(self.call('inbox', 'bob')['total'], 0)
    def test_selected_manager_does_not_stall_on_unrelated_retained_receipts(self):
        self.call('join', 'bob', {'role': 'steward', 'topics': ['@bob']})
        self.post('Unrelated work')
        self.cli('alice', 'send', 'bob', 'Please review', '--ask')
        record = pathlib.Path(self.home) / 'turns.jsonl'
        result = self.run_agent(record, '--max-turns', '1')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.call('inbox', 'bob')['total'], 1)
        turns = [json.loads(line) for line in record.read_text().splitlines()]
        self.assertEqual(turns[0]['attention']['total'], 1)
        self.assertNotIn('available', turns[0])
        # Acknowledged open questions don't trigger another startup or idle turn.
        result = self.run_agent(record, '--idle-timeout', '0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(record.read_text().splitlines()), 1)
    def test_involved_wait_ignores_broadcasts_but_wakes_for_outgoing_reply(self):
        self.call('join', 'bob', {'role': 'steward', 'topics': []})
        self.post('Noise')
        request = self.cli('bob', 'send', 'alice', 'Please verify', '--ask')
        with Peer(self.home) as waiter:
            waiter.send('wait', 'bob', {'timeout': 4, 'selection': 'involved'})
            # A response before this reply is a selection bug, even with existing backlog.
            time.sleep(.1)
            answer = self.cli('alice', 'reply', str(request['card']['id']), 'Verified')
            result = waiter.receive()
            self.assertTrue(result['ok'], result)
            self.assertEqual(result['data']['total'], 1)
            self.assertEqual(result['data']['items'][0]['through_seq'], answer['event_seq'])
        with Peer(self.home) as waiter:
            result = waiter.call('wait', 'bob', {'timeout': 0, 'selection': 'typo'})
            self.assertFalse(result['ok'])
    def test_batch_receipts_cli_rejects_wrong_store_without_partial_ack(self):
        self.cli('alice', 'send', 'bob', 'One')
        self.cli('alice', 'send', 'bob', 'Two')
        receipts = [x['receipt'] for x in self.call('inbox', 'bob')['items']]
        bad = json.loads(json.dumps(receipts))
        bad[1]['store_id'] = 'wrong-store'
        result = subprocess.run([str(BINARY), '--home', self.home, '--as', 'bob',
            'ack', '--receipts', '-'], input=json.dumps(bad), text=True, capture_output=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.call('inbox', 'bob')['total'], 2)
        self.cli('bob', 'ack', '--receipts', '-', body=json.dumps(receipts))
        self.assertEqual(self.call('inbox', 'bob')['total'], 0)
    def test_duplicate_controller_and_child_timeout_are_visible(self):
        record = pathlib.Path(self.home) / 'turns.jsonl'
        runner = subprocess.Popen([str(BINARY), '--home', self.home, '--as', 'bob',
            'drive', '--bootstrap', '--child-timeout', '2', '--', sys.executable,
            '-c', 'import time; time.sleep(60)'], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            self.await_controller('bob', 'running')
            duplicate = self.run_agent(record, '--bootstrap', '--max-turns', '1')
            self.assertNotEqual(duplicate.returncode, 0)
            self.assertIn('controller_busy', duplicate.stderr)
            self.assertFalse(record.exists())
            stdout, stderr = runner.communicate(timeout=6)
            self.assertNotEqual(runner.returncode, 0, stdout + stderr)
            self.assertIn('child_timeout', stderr)
            self.assertEqual(self.controller('bob')['state'], 'failed')
            self.assertEqual(self.controller('bob')['reason'], 'child_timeout')
        finally:
            if runner.poll() is None: runner.kill()
            runner.communicate(timeout=5)
    def test_busy_and_idle_controllers_refresh_presence_without_model_polling(self):
        runners = []
        record = pathlib.Path(self.home) / 'idle-turns.jsonl'
        try:
            for actor, options, child in [
                ('alice', ['--idle-timeout', '60'], [str(AGENT), str(record)]),
                ('bob', ['--bootstrap', '--max-turns', '1'], ['-c', 'import time; time.sleep(32)']),
            ]:
                runners.append(subprocess.Popen([str(BINARY), '--home', self.home, '--as', actor,
                    'drive', *options, '--', sys.executable, *child], text=True,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE))
            idle = self.await_controller('alice', 'waiting')
            busy = self.await_controller('bob', 'running')
            # One real heartbeat interval; expiry/fencing is also tested with a fake clock in Rust.
            time.sleep(31)
            for actor, before in [('alice', idle), ('bob', busy)]:
                now = self.controller(actor)
                self.assertGreater(now['updated_ms'], before['updated_ms'])
                self.assertTrue(now['live'])
            self.assertFalse(record.exists(), 'idle agent was invoked to maintain presence')
            # Explicit leave stops the waiting controller; it must not rejoin itself.
            self.call('leave', 'alice')
            for runner in runners:
                runner.communicate(timeout=6)
            self.assertNotEqual(runners[0].returncode, 0)
            self.assertEqual(runners[1].returncode, 0)
            self.assertEqual(self.controller('alice')['state'], 'stopped')
            self.assertEqual(self.controller('bob')['state'], 'stopped')
        finally:
            for runner in runners:
                if runner.poll() is None: runner.kill()
                runner.communicate(timeout=5)
    def test_hooks_do_not_reinject_context_inside_drive(self):
        self.post()
        result = subprocess.run([str(BINARY), '--home', self.home, '--as', 'bob', 'hook'],
            input=json.dumps({'hook_event_name': 'SessionStart'}), text=True, capture_output=True,
            env={**os.environ, 'FRAY_DRIVE': '1'}, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {})
        self.assertEqual(self.call('inbox', 'bob')['total'], 1)
    def test_explicit_all_selection_processes_discovery_and_logs_no_token_estimate(self):
        self.post('Discovery')
        record = pathlib.Path(self.home) / 'turns.jsonl'
        result = self.run_agent(record, '--selection', 'all', '--max-turns', '1')
        self.assertEqual(result.returncode, 0, result.stderr)
        turns = [json.loads(line) for line in record.read_text().splitlines()]
        self.assertEqual(turns[0]['attention']['total'], 1)
        metrics = [json.loads(line.removeprefix('fray drive: '))
                   for line in result.stderr.splitlines() if line.startswith('fray drive: ')]
        self.assertEqual(metrics[0]['prompt_bytes'], turns[0]['prompt_bytes'])
        self.assertIsNone(metrics[0]['provider_usage'])
        self.assertEqual(metrics[0]['exit_reason'], 'success')
        self.assertNotIn('argv', metrics[0])
    def test_oversize_receipt_fails_before_spawning_instead_of_empty_turns(self):
        sent = self.cli('alice', 'send', 'bob', '🧠' * 300, '--ask')
        for _ in range(2):
            self.cli('alice', 'reply', str(sent['card']['id']), '🧠' * 300)
        record = pathlib.Path(self.home) / 'turns.jsonl'
        result = self.run_agent(record, '--budget', '2000', '--max-turns', '1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('prompt_budget', result.stderr)
        self.assertFalse(record.exists())
        self.assertEqual(self.call('inbox', 'bob')['total'], 1)
        result = self.run_agent(record, '--budget', '8000', '--max-turns', '1')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLessEqual(json.loads(record.read_text())['prompt_bytes'], 8000)
        self.assertEqual(self.call('inbox', 'bob')['total'], 0)
    def test_child_failure_does_not_report_a_live_controller_or_ack(self):
        self.cli('alice', 'send', 'bob', 'Pending')
        result = subprocess.run([str(BINARY), '--home', self.home, '--as', 'bob', 'drive',
            '--', sys.executable, '-c', 'raise SystemExit(7)'], text=True, capture_output=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('agent_exit', result.stderr)
        self.assertEqual(self.controller('bob')['state'], 'failed')
        self.assertFalse(self.controller('bob')['live'])
        self.assertEqual(self.call('inbox', 'bob')['total'], 1)
    def test_old_daemon_is_rejected_without_shutdown_or_registration(self):
        commands = [
            ['inbox'], ['wait', '--timeout', '0'], ['start'], ['agents'], ['join'],
            ['post', 'Must not publish'], ['ack', '1', '--through', '1'],
            ['rpc', '{"op":"inbox","args":{"selection":"involved"}}'],
            ['watch', '--reconnect'],
            ['enter', '--', sys.executable, '-c', 'raise SystemExit(77)'],
            ['drive', '--bootstrap', '--', sys.executable, '-c', 'raise SystemExit(77)'],
        ]
        for command in commands:
            with self.subTest(command=command), tempfile.TemporaryDirectory(prefix="fray-v1-'", dir='/tmp') as home:
                self.assert_old_daemon_rejected(home, command)
    def assert_old_daemon_rejected(self, home, command):
        # start/enter/drive canonicalize the home; use that same exact path for
        # every command so macOS's /tmp -> /private/tmp alias isn't a mismatch.
        home = str(pathlib.Path(home).resolve())
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.bind(home + '/bus.sock')
            listener.listen(1)
            listener.settimeout(5)
            requests = []
            def legacy_ping():
                peer, _ = listener.accept()
                peer.settimeout(5)
                with peer, peer.makefile('rwb', buffering=0) as stream:
                    requests.append(json.loads(stream.readline()))
                    stream.write(b'{"ok":true,"data":{"version":"0.1.0","protocol_version":1}}\n')
                    # Incompatibility must close the connection before any real operation.
                    followup = stream.readline()
                    if followup:
                        requests.append(json.loads(followup))
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(legacy_ping)
                result = subprocess.run([str(BINARY), '--home', home, '--as', 'bob',
                    *command],
                    text=True, capture_output=True, timeout=5)
                future.result(timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('protocol_version', result.stderr)
            self.assertIn('0.1.0', result.stderr)
            self.assertIn('(protocol 2)', result.stderr)
            self.assertIn('stop && fray --home', result.stderr)
            # Shell quoting must preserve the exact home, including an apostrophe.
            import shlex
            recovery = shlex.split(result.stderr.split('run: ', 1)[1])
            self.assertEqual(recovery, ['fray', '--home', home, 'stop', '&&', 'fray', '--home', home, 'start'])
            self.assertEqual([r['op'] for r in requests], ['ping'])
            self.assertTrue(pathlib.Path(home, 'bus.sock').exists())
            self.assertFalse(pathlib.Path(home, 'daemon.log').exists())
            self.assertFalse(pathlib.Path(home, 'state.db').exists())
    def test_watch_reconnect_checks_protocol_before_resubscribing(self):
        with tempfile.TemporaryDirectory(prefix='fray-watch-', dir='/tmp') as home:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
                listener.bind(home + '/bus.sock')
                listener.listen(2)
                listener.settimeout(5)
                requests = []
                def replaced_daemon():
                    for protocol in [2, 1]:
                        peer, _ = listener.accept()
                        peer.settimeout(5)
                        connection = []
                        with peer, peer.makefile('rwb', buffering=0) as stream:
                            connection.append(json.loads(stream.readline()))
                            stream.write((json.dumps({'ok': True, 'data': {'version': 'fixture', 'protocol_version': protocol}}) + '\n').encode())
                            line = stream.readline()
                            if line:
                                connection.append(json.loads(line))
                                stream.write(b'{"ok":true,"data":{"type":"ready","store_id":"same-store","cursor":4}}\n')
                        requests.append(connection)
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    future = pool.submit(replaced_daemon)
                    result = subprocess.run([str(BINARY), '--home', home, 'watch', '--reconnect'],
                        text=True, capture_output=True, timeout=8)
                    future.result(timeout=5)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('protocol_version', result.stderr)
                self.assertEqual([[r['op'] for r in conn] for conn in requests], [['ping', 'watch'], ['ping']])
                self.assertEqual(len(result.stdout.splitlines()), 1)
    def test_wait_wakes_for_committed_work(self):
        with Peer(self.home) as waiter:
            waiter.send('wait','bob',{'timeout':4})
            before=time.perf_counter();r=self.post();reply=waiter.receive()
            self.assertTrue(reply['ok']);self.assertEqual(reply['data']['items'][0]['through_seq'],r['event_seq'])
            self.assertEqual(reply['data']['store_id'], r['store_id'])
            # Functional timeout, NOT a sub-millisecond performance assertion.
            self.assertLess(time.perf_counter()-before,4)
    def test_wait_wakes_when_scope_change_seeds_existing_attention(self):
        self.call('join', 'bob', {'topics': ['tests']})
        self.call('post', 'alice', {'topic': 'parser', 'title': 'Design', 'summary': 'Current parser decision'})
        with Peer(self.home) as waiter:
            waiter.send('wait', 'bob', {'timeout': 4})
            self.call('join', 'bob', {'topics': ['parser']})
            reply = waiter.receive()
            self.assertTrue(reply['ok'])
            self.assertFalse(reply['data']['timed_out'])
            self.assertEqual(reply['data']['total'], 1)
    def test_watch_replays_then_follows(self):
        r=self.post()
        with Peer(self.home) as w:
            w.send('watch',args={'after':0});self.assertEqual(w.receive()['data']['type'],'ready')
            self.assertEqual(w.receive()['data']['event']['seq'],r['event_seq'])
            w.receive() # batch checkpoint
            n=self.post('Second')
            self.assertEqual(w.receive()['data']['event']['seq'],n['event_seq'])
    def test_watch_startup_race_never_loses_events(self):
        for n in range(12):
            after=self.call('ping')['cursor']
            with Peer(self.home) as w:
                w.send('watch',args={'after':after})
                r=self.post(f'race {n}')
                self.assertEqual(w.receive()['data']['type'],'ready')
                received=w.receive()['data']
                self.assertEqual(received['event']['seq'],r['event_seq'])
    def test_claim_race_has_exactly_one_winner(self):
        r=self.post();id_=r['card']['id'];barrier=threading.Barrier(2)
        def claim(actor):
            with Peer(self.home) as p:
                barrier.wait();return p.call('claim',actor,{'id':id_})
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            results=list(pool.map(claim,['alice','bob']))
        self.assertEqual(sum(x['ok'] for x in results),1)
        self.assertEqual([x['error']['code'] for x in results if not x['ok']],['claimed'])
    def test_crash_restart_preserves_committed_heads_and_inbox(self):
        r=self.post();store_id=r['store_id'];self.server.kill();self.server.wait();self.launch()
        self.assertEqual(self.call('ping')['store_id'],store_id)
        self.assertEqual(self.call('show',args={'id':r['card']['id']})['card']['title'],'Work')
        self.assertEqual(self.call('inbox','bob')['total'],1)
    def test_same_key_retry_after_restart(self):
        args={'title':'Retry','summary':'one durable event'}
        first=self.call('post','alice',args,'retry-one');self.server.kill();self.server.wait();self.launch()
        second=self.call('post','alice',args,'retry-one')
        self.assertEqual(first,second);self.assertEqual(self.call('ping')['cursor'],first['event_seq'])
    def test_malformed_frame_does_not_kill_daemon(self):
        with Peer(self.home) as p:
            p.file.write(b'{bad json}\n');self.assertFalse(p.receive()['ok'])
            self.assertTrue(p.call('ping')['ok'])
        self.call('ping')
    def test_invalid_shutdown_returns_error_and_keeps_connection_usable(self):
        with Peer(self.home) as peer:
            result = peer.call('shutdown', args={'unexpected': True})
            self.assertFalse(result['ok'])
            self.assertEqual(result['error']['code'], 'invalid')
            self.assertTrue(peer.call('ping')['ok'])
    def test_cli_and_claude_hook_contract(self):
        self.post(assignee="bob", priority=1)
        env={**os.environ,'FRAY_HOME':self.home,'FRAY_AGENT':'bob'}
        r=subprocess.run([str(BINARY),'hook'],input=json.dumps({'hook_event_name':'PostToolUse'}),text=True,capture_output=True,env=env,timeout=10)
        self.assertEqual(r.returncode,0,r.stderr)
        obj=json.loads(r.stdout)
        self.assertEqual(obj['hookSpecificOutput']['hookEventName'],'PostToolUse')
        self.assertIn('Fray public project state',obj['hookSpecificOutput']['additionalContext'])
        self.assertEqual(self.call('inbox','bob')['total'],1) # hook exposure != ACK
        stop=subprocess.run([str(BINARY),'hook'],input=json.dumps({'hook_event_name':'Stop','stop_hook_active':True}),text=True,capture_output=True,env=env,timeout=10)
        self.assertEqual(json.loads(stop.stdout),{})
    def test_drive_supplies_context_to_child(self):
        self.post(assignee='bob')
        child=pathlib.Path(self.home)/'fake_agent.py'
        child.write_text('''import json, os, socket, sys
prompt=sys.stdin.read()
assert "CURRENT PROJECT STATE" in prompt
assert "Fray agent bob" in prompt
s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.connect(os.environ['FRAY_HOME']+'/bus.sock')
f=s.makefile('rwb',buffering=0)
def call(op,args):
 f.write((json.dumps({'op':op,'actor':os.environ['FRAY_AGENT'],'args':args})+'\\n').encode());return json.loads(f.readline())['data']
for item in call('inbox',{})['items']:
 call('ack',{'id':item['card']['id'],'through':item['through_seq']})
''')
        r=subprocess.run([str(BINARY),'--home',self.home,'--as','bob','drive','--max-turns','1','--',sys.executable,str(child)],text=True,capture_output=True,timeout=10)
        self.assertEqual(r.returncode,0,r.stderr);self.assertEqual(self.call('inbox','bob')['total'],0)
    def test_cli_byte_budget(self):
        for i in range(15): self.post(f'task {i}',summary='🧠'*300)
        r=subprocess.run([str(BINARY),'--home',self.home,'--as','bob','--json','brief','--budget','2000'],capture_output=True,timeout=10)
        self.assertEqual(r.returncode,0,r.stderr)
        self.assertLessEqual(len(r.stdout.rstrip(b'\n')),2000)
        self.assertTrue(json.loads(r.stdout)['budget_truncated'])

if __name__=='__main__': unittest.main(verbosity=2)
