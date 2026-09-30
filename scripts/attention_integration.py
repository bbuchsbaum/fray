#!/usr/bin/env python3
"""Host-neutral attention IPC and thin adapter contracts, using only fixtures."""
import contextlib
import json
import os
import pathlib
import queue
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unittest

import integration as base

BINARY = base.BINARY
ADAPTER = pathlib.Path(__file__).resolve().parents[1] / 'integrations/claude-plugin/scripts/attention.py'


class Attention(unittest.TestCase):
    setUp = base.Integration.setUp
    tearDown = base.Integration.tearDown
    launch = base.Integration.launch
    call = base.Integration.call
    cli = base.Integration.cli
    post = base.Integration.post

    def listener(self, actor='bob'):
        return next(a for a in self.call('agents')['items'] if a['name'] == actor)['listener']

    def armed(self, actor='bob'):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            state = self.listener(actor)
            if state and state['state'] == 'armed':
                return state
            time.sleep(.02)
        self.fail('listener never armed')

    @contextlib.contextmanager
    def watch(self, *args, adapter=False, actor='bob', home=None):
        home = home or self.home
        env = {**os.environ, 'FRAY_HOME':home, 'FRAY_AGENT':actor, 'FRAY_BIN':str(BINARY)}
        env.pop('FRAY_DRIVE', None)
        env.pop('FRAY_SELECTION', None)
        cmd = ([sys.executable, str(ADAPTER), 'monitor'] if adapter else
               [str(BINARY), '--home', home, '--as', actor, 'watch', '--attention', *args])
        process = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
        lines = queue.Queue()
        def reader():
            for line in process.stdout:
                lines.put(json.loads(line))
        thread = threading.Thread(target=reader)
        thread.start()
        try:
            yield process, lines
        finally:
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            thread.join(timeout=3)
            process.stdout.close()
            process.stderr.close()

    def send(self, body='Handle this', to='bob', **extra):
        return self.call('send', 'alice', {'to':to,'body':body,**extra})

    def test_quiet_heartbeat_and_unrelated_traffic_never_reach_stdout(self):
        self.call('join', 'bob', {'role':'steward', 'topics':[]})
        with self.watch('--timeout', '18') as (process, lines):
            self.armed()
            self.post('Unrelated broadcast')
            with self.assertRaises(queue.Empty):
                lines.get(timeout=15.5)  # Cross a real transport heartbeat.
            self.assertIsNone(process.poll())
            sent = self.send()
            packet = lines.get(timeout=2)
            self.assertEqual(packet['items'][0]['receipt']['id'], sent['card']['id'])

    def test_fixed_window_coalesces_burst_and_receipts_stay_pending(self):
        with self.watch('--settle-ms', '300') as (_, lines):
            self.armed()
            sent = [self.send(f'Message {n}') for n in range(3)]
            packet = lines.get(timeout=3)
            self.assertEqual({i['card']['id'] for i in packet['items']}, {s['card']['id'] for s in sent})
            self.assertEqual(self.call('inbox', 'bob')['total'], 3)
            with self.assertRaises(queue.Empty):
                lines.get(timeout=.5)
            self.call('ack', 'bob', {'receipts':[i['receipt'] for i in packet['items']]})
            with self.assertRaises(queue.Empty):
                lines.get(timeout=.3)

    def test_urgent_bypasses_settlement_and_continuous_traffic_cannot_starve(self):
        with self.watch('--settle-ms', '5000') as (_, lines):
            self.armed()
            self.send('Urgent', priority=1)
            self.assertEqual(lines.get(timeout=2)['items'][0]['card']['priority'], 1)
        # Use the other identity to exercise bidirectional routing as well.
        with self.watch('--settle-ms', '250', actor='alice') as (_, lines):
            self.armed('alice')
            stop = threading.Event()
            def traffic():
                while not stop.wait(.04):
                    self.call('send', 'bob', {'to':'alice','body':'Burst'})
            thread = threading.Thread(target=traffic)
            thread.start()
            try:
                self.assertTrue(lines.get(timeout=2)['items'])
                self.assertTrue(thread.is_alive())
            finally:
                stop.set()
                thread.join()

    def test_priority_backlog_and_newly_followed_old_card_are_not_skipped(self):
        old = self.post('Older invisible card')
        ordinary = self.send('Older ordinary', priority=3)
        urgent = self.send('New urgent', priority=0)
        with self.watch('--limit', '1', '--settle-ms', '0') as (_, lines):
            self.assertEqual(lines.get(timeout=3)['items'][0]['card']['id'], urgent['card']['id'])
            self.assertEqual(lines.get(timeout=3)['items'][0]['card']['id'], ordinary['card']['id'])
            self.call('follow', 'bob', {'id':old['card']['id']})
            self.assertEqual(lines.get(timeout=3)['items'][0]['card']['id'], old['card']['id'])

    def test_reconnect_after_daemon_crash_recovers_unacked_exact_versions(self):
        self.send()
        with self.watch('--reconnect') as (_, lines):
            first = lines.get(timeout=3)
            self.server.kill()
            self.server.wait()
            self.launch()
            replay = lines.get(timeout=5)
            self.assertEqual(first['items'][0]['receipt'], replay['items'][0]['receipt'])
            self.call('ack', 'bob', {'receipts':[replay['items'][0]['receipt']]})
            self.send('After recovery')
            packet = lines.get(timeout=3)
            self.assertEqual(len(packet['items']),1)
            self.assertEqual(packet['items'][0]['messages'][0]['body'], 'After recovery')

    def test_opt_in_control_frames_fence_disconnect_and_reconnect(self):
        with self.watch('--include-control', '--reconnect') as (_, lines):
            ready = lines.get(timeout=3)
            self.assertEqual((ready['type'], ready['agent'], ready['control_version']),
                             ('ready', 'bob', 1))
            self.send()
            first = lines.get(timeout=3)
            self.assertEqual(first['type'], 'attention')
            self.server.kill()
            self.server.wait()
            disconnected = lines.get(timeout=3)
            self.assertEqual(disconnected['type'], 'disconnected')
            self.assertEqual(disconnected['store_id'], ready['store_id'])
            self.launch()
            rearmed = lines.get(timeout=5)
            self.assertEqual(rearmed, ready)
            replay = lines.get(timeout=3)
            self.assertEqual(replay['items'][0]['receipt'], first['items'][0]['receipt'])

    def test_control_readiness_is_not_emitted_for_a_left_identity(self):
        self.call('leave', 'bob')
        result = subprocess.run([str(BINARY), '--home', self.home, '--as', 'bob',
                                 'watch', '--attention', '--include-control'],
                                capture_output=True, text=True, timeout=3)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, '')
        self.assertIn('not_joined', result.stderr)

    def test_reconnect_rejects_replaced_database(self):
        self.send()
        with self.watch('--reconnect') as (process, lines):
            lines.get(timeout=3)
            self.call('shutdown')
            self.server.wait(timeout=3)
            for name in ['state.db', 'state.db-wal', 'state.db-shm']:
                pathlib.Path(self.home, name).unlink(missing_ok=True)
            self.launch()
            self.call('join','alice')
            self.call('join','bob')
            self.send('Different store')
            self.assertNotEqual(process.wait(timeout=5),0)
            self.assertIn('store_changed',process.stderr.read())
            self.assertTrue(lines.empty())
            self.assertIsNone(self.listener())  # identity checked before subscribing

    def test_once_timeout_and_explicit_leave_have_distinct_outcomes(self):
        result = subprocess.run([str(BINARY),'--home',self.home,'--as','bob','--json','watch','--attention','--once','--timeout','1'],capture_output=True,text=True,timeout=4)
        self.assertEqual(result.returncode,3,result.stderr)
        self.assertEqual(result.stdout,'')
        self.assertEqual(self.listener()['state'],'stopped')
        self.send()
        result = self.cli('bob','watch','--attention','--once')
        self.assertEqual(result['type'],'attention')
        self.assertEqual(self.call('inbox','bob')['total'],1)
        with self.watch() as (process, lines):
            lines.get(timeout=3)
            self.call('leave','bob')
            self.assertNotEqual(process.wait(timeout=3),0)
            self.assertEqual(self.listener()['state'],'stopped')

    def test_duplicate_listener_and_driver_cannot_compete(self):
        with self.watch() as (_, lines):
            self.armed()
            duplicate = subprocess.run([str(BINARY),'--home',self.home,'--as','bob','watch','--attention','--once'],capture_output=True,text=True,timeout=4)
            self.assertIn('listener_busy',duplicate.stderr)
            driver = subprocess.run([str(BINARY),'--home',self.home,'--as','bob','drive','--idle-timeout','0','--',sys.executable,'-c','pass'],capture_output=True,text=True,timeout=4)
            self.assertIn('controller_busy',driver.stderr)
            self.assertTrue(lines.empty())

    def test_wait_filters_match_attention_and_do_not_consume_hidden_receipts(self):
        self.call('join','bob',{'role':'steward'})
        self.post('Unassigned')
        self.send('Direct')
        expected = self.call('inbox','bob',{'selection':'involved','addressed_to_me':True,'unresolved':True})
        waited = self.cli('bob','wait','--timeout','0','--selection','involved','--addressed-to-me','--unresolved')
        packet = self.cli('bob','watch','--attention','--once','--addressed-to-me','--unresolved')
        for result in [waited,packet]:
            self.assertEqual([i['receipt'] for i in result['items']],[i['receipt'] for i in expected['items']])
        self.assertEqual(self.call('inbox','bob')['total'],2)

    def test_adapter_monitor_delegates_to_the_same_generic_stream(self):
        with self.watch(adapter=True) as (_, lines):
            self.armed()
            self.send('Adapter input')
            notice=lines.get(timeout=3)
            self.assertEqual(notice['type'],'attention_notice')
            batch=self.cli('bob','batch',notice['batch'])
            self.assertEqual(batch['items'][0]['receipt']['agent'],'bob')
            self.assertLessEqual(len(json.dumps(notice,separators=(',',':'),ensure_ascii=False).encode())+1,768)
            self.assertEqual(self.listener()['selection']['activation']['mode'],'native-monitor')

    def test_adapter_rewake_exit_mapping_and_managed_child_suppression(self):
        self.send('One-shot wake')
        env = {**os.environ,'FRAY_HOME':self.home,'FRAY_AGENT':'bob','FRAY_BIN':str(BINARY)}
        env.pop('FRAY_DRIVE',None)
        result = subprocess.run([sys.executable,str(ADAPTER),'rewake'],input=json.dumps({'hook_event_name':'SessionStart'}),capture_output=True,text=True,env=env,timeout=4)
        self.assertEqual(result.returncode,2,result.stderr)
        self.assertEqual(result.stdout,'')
        self.assertEqual(json.loads(result.stderr)['type'],'attention')
        self.assertEqual(self.call('inbox','bob')['total'],1)
        env['FRAY_DRIVE']='1'
        result = subprocess.run([sys.executable,str(ADAPTER),'monitor'],capture_output=True,text=True,env=env,timeout=4)
        self.assertEqual((result.returncode,result.stdout,result.stderr),(0,'',''))

    def test_broadcast_watch_still_accepts_inherited_attention_selection(self):
        env = {**os.environ,'FRAY_SELECTION':'involved'}
        process = subprocess.Popen([str(BINARY),'--home',self.home,'watch'],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            self.assertEqual(json.loads(process.stdout.readline())['type'],'ready')
        finally:
            process.terminate()
            process.wait(timeout=3)
            process.stdout.close()
            process.stderr.close()

    def test_attention_protocol_has_no_global_offset_and_errors_stay_off_stdout(self):
        with base.Peer(self.home) as peer:
            response=peer.call('watch_attention','bob',{'run_id':'fixture','after':100})
            self.assertFalse(response['ok'])
            self.assertIn('unknown field',response['error']['message'])
        result=subprocess.run([str(BINARY),'--home',self.home,'--as','bob','--json','watch','--attention','--budget','1'],capture_output=True,text=True,timeout=3)
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(result.stdout,'')
        self.assertIn('budget',result.stderr)

    def test_killed_idle_listener_is_released_and_replaced_within_one_second(self):
        with self.watch() as (process,_):
            self.armed()
            started=time.monotonic()
            process.kill()
            process.wait(timeout=1)
            while self.listener()['state']=='armed' and time.monotonic()-started<.8:
                time.sleep(.01)
            self.assertEqual(self.listener()['state'],'stopped')
            with self.watch() as (_,lines):
                self.armed()
                self.assertLess(time.monotonic()-started,1)
                self.send('After killed consumer')
                self.assertTrue(lines.get(timeout=2)['items'])

    def test_expired_own_connection_reconnects_but_leave_does_not(self):
        with self.watch('--reconnect') as (process,lines):
            self.armed()
            # Simulate wall-clock advancing over sleep while Instant's heartbeat
            # has not yet fired. This fixture changes only its own temporary DB.
            with contextlib.closing(sqlite3.connect(str(pathlib.Path(self.home)/'state.db'))) as db, db:
                db.execute('UPDATE listeners SET updated_ms=updated_ms-60000 WHERE agent=?',('bob',))
            self.send('After simulated sleep')
            self.assertEqual(lines.get(timeout=4)['items'][0]['messages'][0]['body'],'After simulated sleep')
            self.call('leave','bob')
            self.assertNotEqual(process.wait(timeout=3),0)
            self.assertEqual(self.listener()['state'],'stopped')

    def test_attention_stream_rejects_inbound_frames_instead_of_swallowing_them(self):
        with base.Peer(self.home) as peer:
            peer.send('watch_attention','bob',{'run_id':'fixture','selection':'involved'})
            self.assertEqual(peer.receive()['data']['type'],'ready')
            peer.send('ack','bob',{'id':1,'through':1})
            failure=peer.receive()
            self.assertFalse(failure['ok'])
            self.assertEqual(failure['error']['code'],'protocol')
        self.assertEqual(self.listener()['state'],'stopped')

    def test_wait_none_exits_only_for_selected_attention_and_resumes_from_exact_ack(self):
        args=[str(BINARY),'--home',self.home,'--as','bob','--json','wait','--timeout','none',
              '--selection','involved','--addressed-to-me','--kinds','question,objection','--min-priority','p2']
        process=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            self.send('Hidden ordinary note')
            self.send('Hidden low urgency',ask=True,priority=3)
            with self.assertRaises(subprocess.TimeoutExpired):
                process.wait(timeout=.2)
            selected=self.send('Selected question',ask=True,priority=2)
            out,err=process.communicate(timeout=3)
            self.assertEqual(process.returncode,0,err)
            page=json.loads(out)
            self.assertFalse(page['timed_out'])
            self.assertEqual([i['card']['id'] for i in page['items']],[selected['card']['id']])
            self.assertEqual(self.cli('bob',*args[6:])['items'][0]['receipt'],page['items'][0]['receipt'])
            self.call('ack','bob',{'receipts':[page['items'][0]['receipt']]})
            timed=subprocess.run([*args[:args.index('none')],'0',*args[args.index('none')+1:]],capture_output=True,text=True,timeout=3)
            self.assertEqual(timed.returncode,3,timed.stderr)
            self.assertTrue(json.loads(timed.stdout)['timed_out'])
            self.assertEqual(self.call('inbox','bob')['total'],2)
        finally:
            if process.poll() is None: process.kill()
            process.communicate(timeout=3)

    def test_wait_none_daemon_failure_and_cancel_release_connections(self):
        args=[str(BINARY),'--home',self.home,'--as','bob','--json','wait','--timeout','none']
        # More cancellations than the connection cap must not strand indefinite waiters.
        for _ in range(140):
            with base.Peer(self.home) as peer:
                peer.send('wait','bob',{'timeout':None})
                time.sleep(.003)
        process=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            with self.assertRaises(subprocess.TimeoutExpired): process.wait(timeout=.1)
            self.server.kill(); self.server.wait(timeout=3)
            out,err=process.communicate(timeout=3)
            self.assertEqual(process.returncode,4,out+err)
            self.assertEqual(json.loads(out)['error']['code'],'unavailable')
            unavailable=subprocess.run(args,capture_output=True,text=True,timeout=3)
            self.assertEqual(unavailable.returncode,4)
        finally:
            if process.poll() is None: process.kill()
            process.communicate(timeout=3)

    def test_kind_and_priority_selection_match_wait_inbox_and_watch(self):
        self.send('ordinary note')
        self.send('low urgency question',ask=True,priority=3)
        question=self.send('important question',ask=True,priority=1)
        filters=['--selection','involved','--kinds','question,objection','--min-priority','p2','--addressed-to-me']
        expected=self.cli('bob','inbox',*filters)['items'][0]['receipt']
        self.assertEqual(expected['id'],question['card']['id'])
        self.assertEqual(self.cli('bob','wait','--timeout','0',*filters)['items'][0]['receipt'],expected)
        with self.watch('--once',*filters) as (_,lines):
            packet=lines.get(timeout=3)
            self.assertEqual([item['receipt'] for item in packet['items']],[expected])

    def test_compact_notification_retains_receipt_and_fetches_full_body(self):
        body='Full context 🧠\n' * 200
        sent=self.send(body,ask=True)
        with self.watch('--notification','--once') as (_,lines):
            notice=lines.get(timeout=3)
            self.assertEqual(notice['type'],'attention_notice')
            batch=self.cli('bob','batch',notice['batch'])
            self.assertEqual(batch['items'][0]['receipt']['through_seq'],sent['event_seq'])
            self.assertEqual(batch['items'][0]['receipt']['store_id'],sent['store_id'])
            self.assertLessEqual(len(json.dumps(notice,separators=(',',':'),ensure_ascii=False).encode())+1,768)
        thread=self.call('show','bob',{'id':sent['card']['id'],'history':True})
        self.assertEqual(thread['history'][0]['payload']['detail']['body'],body)
        self.assertEqual(self.call('inbox','bob')['total'],1)

    def test_notification_previews_latest_reply_instead_of_opening_title(self):
        sent = self.send('Opening hello', ask=True)
        reply = self.call('annotate', 'alice', {
            'id': sent['card']['id'], 'kind': 'evidence',
            'body': '\nThe regression now passes\nRaw log: fixture.log',
        })
        with self.watch('--notification', '--once') as (_, lines):
            notice = lines.get(timeout=3)
            self.assertEqual(notice['titles'][0]['title'], 'Opening hello')
            self.assertEqual(notice['titles'][0]['preview']['kind'], 'evidence')
            self.assertEqual(notice['titles'][0]['preview']['body'], 'The regression now passes')
            self.assertEqual(notice['titles'][0]['preview']['seq'], reply['event_seq'])
        batch = self.cli('bob', 'batch', notice['batch'])
        self.assertEqual(batch['items'][0]['receipt']['through_seq'], reply['event_seq'])
        self.assertFalse(batch['items'][0]['handled'])

    def test_review_candidate_and_stale_verdict_survive_bounded_wake(self):
        a='manifest:'+('a'*64);b='manifest:'+('b'*64);c='manifest:'+('c'*64)
        review=self.call('review_request','alice',{'to':'bob','title':'Source review','body':'Verify source',
            'baseline':a,'candidate':b})
        card=review['card']['id']
        self.call('annotate','bob',{'id':card,'kind':'evidence','body':'Checked B',
            'review_verdict':{'verdict':'approve','at':b,'expect':1}})
        changed=self.call('review_subject','alice',{'id':card,'at':c,'expect':1})
        with self.watch('--once','--budget','2000') as (_,lines):
            packet=lines.get(timeout=3)
            self.assertLessEqual(len(json.dumps(packet,separators=(',',':'),ensure_ascii=False).encode())+1,2000)
            subject=packet['items'][0]['card']['review']
            self.assertEqual((subject['baseline'],subject['candidate'],subject['subject_rev']),(a,c,2))
            self.assertTrue(subject['latest_verdict']['stale'])
            self.assertEqual(packet['items'][0]['receipt']['through_seq'],changed['event_seq'])

    def test_doctor_is_read_only_and_distinguishes_manual_and_expired_activation(self):
        def snapshot():
            with contextlib.closing(sqlite3.connect(str(pathlib.Path(self.home)/'state.db'))) as db:
                return list(db.iterdump())
        before=snapshot()
        report=self.cli('bob','doctor')
        self.assertFalse(report['model_response_guaranteed'])
        self.assertIn('not_listening',[c['code'] for c in report['checks']])
        self.assertEqual(snapshot(),before)
        with self.watch('--activation','manual'):
            self.armed()
            report=self.cli('bob','doctor')
            self.assertTrue(report['listening']['live'])
            self.assertIn('idle_wake_unavailable',[c['code'] for c in report['checks']])
        # Wait for EOF cleanup before rearming with a new run identity.
        deadline=time.monotonic()+2
        while self.listener()['live'] and time.monotonic()<deadline: time.sleep(.01)
        with self.watch('--activation','native-monitor','--activation-expires-ms','1'):
            self.armed()
            report=self.cli('bob','doctor')
            self.assertTrue(report['listening']['activation_expired'])
            self.assertIn('activation_expired',[c['code'] for c in report['checks']])

    def test_doctor_reports_unavailable_without_starting_daemon(self):
        self.server.kill(); self.server.wait(timeout=3)
        report=self.cli('bob','doctor')
        self.assertFalse(report['daemon']['reachable'])
        self.assertEqual(report['status'],'blocked')

    def test_unread_cli_paginates_before_ack_and_keeps_later_reply_pending(self):
        sent=self.send('Initial contract',ask=True)
        card=sent['card']['id']
        for body in ['Evidence one','Evidence two','Evidence three']:
            self.call('annotate','alice',{'id':card,'body':body})
        first=self.cli('bob','thread',str(card),'--unread','--limit','2')
        second=self.cli('bob','thread',str(card),'--unread','--limit','2','--after',str(first['next_after']))
        self.assertGreater(second['unread'][0]['seq'],first['next_after'])
        self.assertEqual(second['ack_seq'],0)
        self.assertFalse(second['more'])
        self.assertIn('batch',second)
        self.call('annotate','alice',{'id':card,'body':'A newer peer reply'})
        ack=self.cli('bob','ack','--batch',second['batch'])
        self.assertEqual(ack['acknowledged'][0]['ack_seq'],second['receipt']['through_seq'])
        self.assertTrue(ack['acknowledged'][0]['still_pending'])
        full=self.cli('bob','thread',str(card),'--compact')
        self.assertEqual(full['history'][-1]['body'],'A newer peer reply')
        self.assertIn('receipts',full)

    def test_adapter_fallback_and_doctor_against_old_attention_protocol(self):
        # This fixture models the published pre-batch protocol, including its
        # rejection of activation fields; it does not impersonate a real host.
        errors=[]
        store='a'*32
        receipt={'store_id':store,'agent':'bob','id':1,'through_seq':1}
        packet={'type':'attention','store_id':store,'agent':'bob','more':False,
                'items':[{'card':{'id':1,'title':'Old daemon','priority':2},'receipt':receipt}]}
        with tempfile.TemporaryDirectory(prefix='fray-old-',dir='/tmp') as home, socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(pathlib.Path(home)/'bus.sock'));listener.listen();listener.settimeout(5)
            def mock():
                try:
                    # doctor: ping, agents, inbox; then monitor and rewake.
                    for _ in range(5):
                        connection,_=listener.accept()
                        with connection, connection.makefile('rwb',buffering=0) as wire:
                            connection.settimeout(5)
                            while line:=wire.readline():
                                req=json.loads(line)
                                op=req['op'];args=req.get('args',{})
                                if op=='ping':
                                    data={'protocol_version':2,'version':'0.2.1','capabilities':['attention_stream','attention_filters','wait_indefinite']}
                                elif op=='agents':
                                    data={'items':[{'name':'bob','enabled':True,'listener':None,'controller':None}],'more':False}
                                elif op=='inbox': data={'items':[],'total':0}
                                elif op=='watch_attention':
                                    self.assertNotIn('activation',args)
                                    self.assertNotIn('activation_expires_ms',args)
                                    data=packet
                                else: raise AssertionError(f'unsupported old operation {op}')
                                wire.write((json.dumps({'ok':True,'data':{**data,'store_id':store}})+'\n').encode())
                except Exception as error: errors.append(error)
            server=threading.Thread(target=mock);server.start()
            try:
                result=subprocess.run([str(BINARY),'--home',home,'--as','bob','--json','doctor'],capture_output=True,text=True,timeout=3)
                self.assertEqual(result.returncode,0,result.stdout+result.stderr)
                report=json.loads(result.stdout)
                self.assertEqual(set(report['missing_capabilities']),{'listener_activation','read_batches'})
                self.assertIn('upgrade_required',[c['code'] for c in report['checks']])
                with self.watch(adapter=True,home=home) as (process,lines):
                    notice=lines.get(timeout=3)
                    self.assertEqual(notice['receipt'],receipt)
                    self.assertNotIn('batch',notice)
                    process.terminate();process.wait(timeout=3)
                    self.assertIn('unknown activation',process.stderr.read())
                env={**os.environ,'FRAY_HOME':home,'FRAY_AGENT':'bob','FRAY_BIN':str(BINARY)}
                env.pop('FRAY_DRIVE',None);env.pop('FRAY_SELECTION',None)
                result=subprocess.run([sys.executable,str(ADAPTER),'rewake'],input='{"hook_event_name":"SessionStart"}',capture_output=True,text=True,env=env,timeout=3)
                self.assertEqual(result.returncode,2,result.stdout+result.stderr)
                self.assertIn('unknown activation',result.stderr)
                self.assertIn('"through_seq":1',result.stderr)
            finally:
                server.join(timeout=6)
            self.assertFalse(server.is_alive())
            self.assertEqual(errors,[])


if __name__ == '__main__':
    unittest.main(verbosity=2)
