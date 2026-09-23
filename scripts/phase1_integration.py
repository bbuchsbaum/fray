#!/usr/bin/env python3
"""Phase 1 host/CLI contracts, against a built binary on owned temporary boards."""
import json
import os
import select
import subprocess
import sys
import time
import unittest
import integration as base
BINARY = base.BINARY


class Phase1(unittest.TestCase):
    setUp = base.Integration.setUp
    tearDown = base.Integration.tearDown
    launch = base.Integration.launch
    call = base.Integration.call
    post = base.Integration.post

    def env(self, **extra):
        env = dict(os.environ)
        for key in ('FRAY_SESSION', 'FRAY_AGENT', 'FRAY_HOME', 'FRAY_SELECTION',
                    'FRAY_DRIVE', 'CLAUDE_CODE_SESSION_ID', 'CODEX_THREAD_ID'):
            env.pop(key, None)
        env.update(extra)
        return env

    def command(self, actor, *args, env=None, body=None, code=0):
        result = subprocess.run([str(BINARY), '--home', self.home, '--as', actor,
                                 '--json', *map(str, args)], env=self.env(**(env or {})),
                                input=body, text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        return json.loads(result.stdout) if result.stdout.strip() else None

    def hook(self, event, session='one', active=False, env=None):
        return self.command('bob', 'hook', env=env,
                            body=json.dumps({'hook_event_name': event,
                                             'session_id': session,
                                             'stop_hook_active': active}))

    def test_session_precedence_and_stability_across_fresh_processes(self):
        env = {'CODEX_THREAD_ID': 'one', 'CLAUDE_CODE_SESSION_ID': 'two', 'FRAY_SESSION': 'custom:three'}
        self.command('bob', '--session', 'custom:four', 'join', env=env)
        self.command('bob', '--session', 'custom:four', 'heartbeat', env=env)
        denied = self.command('bob', 'join', env=env, code=1)
        self.assertEqual(denied['error']['code'], 'identity_busy')
        self.command('bob', '--session', 'custom:four', 'leave', env=env)
        self.command('bob', 'join', env=env)
        bob = next(a for a in self.call('agents')['items'] if a['name'] == 'bob')
        self.assertEqual(bob['session']['bound']['session'], 'custom:three')
        self.command('alice', 'join', env={'CODEX_THREAD_ID': 'thread-one'})
        self.command('alice', 'heartbeat', env={'CODEX_THREAD_ID': 'thread-one'})
        denied = self.command('alice', 'heartbeat', env={'CODEX_THREAD_ID': 'thread-two'}, code=1)
        self.assertEqual(denied['error']['code'], 'identity_busy')

    def test_hook_payload_and_claude_shell_share_binding(self):
        self.hook('SessionStart')
        self.command('bob', 'heartbeat', env={'CLAUDE_CODE_SESSION_ID': 'one'})
        result = self.command('bob', 'hook', body=json.dumps({'hook_event_name': 'SessionStart', 'session_id': 'two'}), code=1)
        self.assertEqual(result['error']['code'], 'identity_busy')

    def test_enter_and_drive_children_keep_the_launcher_binding(self):
        child = '''import json, os, subprocess, sys
os.environ['CLAUDE_CODE_SESSION_ID'] = 'different-child-host'
os.environ['CODEX_THREAD_ID'] = 'different-child-thread'
assert os.environ['FRAY_SESSION'] == 'codex:launcher'
def cli(*args):
    r = subprocess.run([sys.argv[1], '--json', *args], capture_output=True, text=True)
    assert r.returncode == 0, r.stdout + r.stderr
    return json.loads(r.stdout)
cli('heartbeat')
if len(sys.argv) > 2:
    sys.stdin.read()
    page = cli('inbox')
    cli('ack', '--receipts', json.dumps([i['receipt'] for i in page['items']]))
'''
        # enter intentionally writes its status to stderr and the child need not
        # emit JSON, so use the subprocess directly for this surface.
        result = subprocess.run([str(BINARY), '--home', self.home, '--as', 'bob',
                                 'enter', '--', sys.executable, '-c', child, str(BINARY)],
                                env=self.env(CODEX_THREAD_ID='launcher'), capture_output=True,
                                text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.post(assignee='bob')
        self.command('bob', 'drive', '--max-turns', 1, '--', sys.executable, '-c',
                     child, BINARY, 'ack', env={'CODEX_THREAD_ID': 'launcher'})
        self.assertEqual(self.call('inbox', 'bob')['total'], 0)

    def test_wait_defaults_involved_and_honors_explicit_all(self):
        self.post()
        self.assertEqual(self.command('bob', 'wait', '--timeout', 0, code=3)['total'], 0)
        self.assertEqual(self.command('bob', 'wait', '--timeout', 0, '--selection', 'all')['total'], 1)
        self.assertEqual(self.command('bob', 'wait', '--timeout', 0, env={'FRAY_SELECTION': 'all'})['total'], 1)

    def test_wait_and_watch_card_only_return_selected_receipts(self):
        first = self.post(assignee='bob')
        second = self.post(title='Second', assignee='bob')
        first_id, second_id = first['card']['id'], second['card']['id']
        wait = self.command('bob', 'wait', '--timeout', 0, '--card', second_id)
        self.assertEqual([i['card']['id'] for i in wait['items']], [second_id])
        stream = self.command('bob', 'watch', '--attention', '--once', '--settle-ms', 0,
                              '--card', first_id)
        self.assertEqual([i['card']['id'] for i in stream['items']], [first_id])
        self.assertEqual(self.call('inbox', 'bob')['total'], 2)

    def test_wait_card_ignores_unrelated_new_traffic(self):
        first = self.post(assignee='bob')
        id_ = first['card']['id']
        self.call('ack', 'bob', {'id': id_, 'through': first['event_seq']})
        waiter = subprocess.Popen([str(BINARY), '--home', self.home, '--as', 'bob', '--json',
                                   'wait', '--card', str(id_), '--timeout', '5'],
                                  env=self.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            self.post(title='Unrelated', assignee='bob')
            self.assertFalse(select.select([waiter.stdout], [], [], .15)[0])
            reply = self.call('annotate', 'alice', {'id': id_, 'body': 'Selected reply'})
            stdout, stderr = waiter.communicate(timeout=8)
            self.assertEqual(waiter.returncode, 0, stderr)
            self.assertEqual(json.loads(stdout)['items'][0]['through_seq'], reply['event_seq'])
        finally:
            if waiter.poll() is None:
                waiter.kill()
            waiter.communicate()
            waiter.stdout.close()
            waiter.stderr.close()

    def test_mute_round_trip_preserves_pending_and_linked_objection(self):
        c = self.post(assignee='bob')['card']
        self.command('bob', 'mute', c['id'])
        self.assertEqual(self.command('bob', 'wait', '--timeout', 0, code=3)['total'], 0)
        objection = self.call('annotate', 'alice', {'id': c['id'], 'kind': 'objection', 'body': 'Evidence missing'})
        wait = self.command('bob', 'wait', '--timeout', 0)
        self.assertEqual([i['card']['id'] for i in wait['items']], [objection['follow_up']['id']])
        self.command('bob', 'unmute', c['id'])
        self.assertEqual(self.call('inbox', 'bob')['total'], 2)

    def test_unmute_wakes_an_already_blocked_waiter_without_a_new_event(self):
        c = self.post(assignee='bob')['card']
        self.command('bob', 'mute', c['id'])
        waiter = subprocess.Popen([str(BINARY), '--home', self.home, '--as', 'bob', '--json',
                                   'wait', '--card', str(c['id']), '--timeout', '5'],
                                  env=self.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 3
            while self.call('ping')['capacity']['long_lived'] == 0:
                self.assertLess(time.monotonic(), deadline, 'waiter never armed')
                time.sleep(.01)
            before = self.call('ping')['cursor']
            self.command('bob', 'unmute', c['id'])
            self.assertEqual(self.call('ping')['cursor'], before)
            stdout, stderr = waiter.communicate(timeout=1)
            self.assertEqual(waiter.returncode, 0, stderr)
            self.assertEqual(json.loads(stdout)['items'][0]['card']['id'], c['id'])
        finally:
            if waiter.poll() is None:
                waiter.kill()
            waiter.communicate()
            waiter.stdout.close()
            waiter.stderr.close()

    def test_assigned_objection_itself_cannot_be_muted(self):
        c = self.post(assignee='bob')['card']
        objection = self.call('annotate', 'alice', {'id': c['id'], 'kind': 'objection', 'body': 'Required evidence'})
        id_ = objection['follow_up']['id']
        refused = self.command('bob', 'mute', id_, code=1)
        self.assertEqual(refused['error']['code'], 'cannot_mute_assigned_request')
        self.assertEqual(self.command('bob', 'wait', '--card', id_, '--timeout', 0)['total'], 1)

    def test_rejoin_gets_a_direct_question_created_and_closed_during_leave(self):
        self.call('leave', 'bob')
        sent = self.call('send', 'alice', {'to': 'bob', 'ask': True, 'body': 'Please review'})
        closed = self.call('patch', 'alice', {'id': sent['card']['id'], 'expect': 1, 'status': 'withdrawn'})
        joined = self.command('bob', 'join')
        self.assertEqual(joined['attention']['total'], 1)
        self.assertEqual(joined['attention']['items'][0]['through_seq'], closed['event_seq'])

    def test_pretool_does_not_consume_posttool_priority_attention(self):
        self.hook('SessionStart')
        self.post(assignee='bob', priority=1)
        self.assertEqual(self.hook('PreToolUse'), {})
        after = self.hook('PostToolUse')
        self.assertIn('Fray public project state', after['hookSpecificOutput']['additionalContext'])
        self.assertEqual(self.call('inbox', 'bob')['total'], 1)
        self.assertEqual(self.hook('PostToolUse'), {})
        self.assertEqual(self.hook('Stop')['decision'], 'block')
        self.assertEqual(self.hook('Stop', active=True), {})

    def test_hook_selection_and_explicit_leave(self):
        self.post()
        start = self.hook('SessionStart')
        data = json.loads(start['hookSpecificOutput']['additionalContext'].split('\n', 1)[1])
        self.assertEqual(data['attention']['total'], 0)
        start = self.hook('SessionStart', env={'FRAY_SELECTION': 'all'})
        data = json.loads(start['hookSpecificOutput']['additionalContext'].split('\n', 1)[1])
        self.assertEqual(data['attention']['total'], 1)
        self.command('bob', 'leave', env={'CLAUDE_CODE_SESSION_ID': 'one'})
        self.assertEqual(self.hook('PostToolUse'), {})
        bob = next(a for a in self.call('agents')['items'] if a['name'] == 'bob')
        self.assertFalse(bob['enabled'])
        self.assertIsNone(bob['session']['bound'])

    def test_stop_warns_once_about_outgoing_requests_without_armed_wake(self):
        self.hook('SessionStart')
        self.command('bob', 'send', 'alice', 'Please review', '--ask', env={'CLAUDE_CODE_SESSION_ID': 'one'})
        stop = self.hook('Stop')
        self.assertEqual(stop['decision'], 'block')
        self.assertIn('no armed listener', stop['reason'])
        self.assertIn('watch --attention --notification', stop['reason'])
        self.assertEqual(self.hook('Stop', active=True), {})


if __name__ == '__main__':
    unittest.main()
