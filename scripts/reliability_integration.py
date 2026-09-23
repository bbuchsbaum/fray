#!/usr/bin/env python3
"""Connection admission/cancellation regression checks on isolated boards."""
import contextlib
import json
import resource
import socket
import subprocess
import time
import unittest
import integration as base


class Reliability(unittest.TestCase):
    setUp=base.Integration.setUp
    tearDown=base.Integration.tearDown
    launch=base.Integration.launch
    call=base.Integration.call

    def wait_count(self, count):
        deadline=time.monotonic()+4
        while time.monotonic()<deadline:
            state=self.call('ping').get('capacity',{})
            if state.get('long_lived')==count: return state
            time.sleep(.01)
        self.fail(f'expected {count} long-lived clients, got {state}')

    def idle_clients(self):
        deadline=time.monotonic()+4
        while time.monotonic()<deadline:
            state=self.call('ping')['capacity']
            if state['clients']==1 and state['long_lived']==0: return
            time.sleep(.01)
        self.fail(f'cancelled clients did not drain: {state}')

    def test_long_waits_reserve_capacity_for_send_inbox_and_ack(self):
        limit=self.call('ping')['capacity']['long_limit']
        with contextlib.ExitStack() as stack:
            peers=[stack.enter_context(base.Peer(self.home)) for _ in range(limit)]
            for peer in peers: peer.send('wait','bob',{'timeout':None})
            state=self.wait_count(limit)
            self.assertEqual(state['short_reserved'],16)
            result=subprocess.run([str(base.BINARY),'--home',self.home,'--as','bob','--json','wait','--timeout','none'],capture_output=True,text=True,timeout=3)
            self.assertEqual(result.returncode,4,result.stdout+result.stderr)
            self.assertEqual(json.loads(result.stdout)['error']['code'],'unavailable')
            self.assertEqual(self.call('inbox','bob')['total'],0)
            sent=self.call('send','alice',{'to':'bob','body':'Keep control RPCs available','ask':True})
            for peer in peers:
                result=peer.receive()
                self.assertTrue(result['ok'],result)
                self.assertEqual(result['data']['items'][0]['card']['id'],sent['card']['id'])
            receipt=self.call('inbox','bob')['items'][0]['receipt']
            self.call('ack','bob',{'receipts':[receipt]})
            self.assertEqual(self.call('inbox','bob')['total'],0)
        self.wait_count(0)

    def test_killed_finite_waits_release_slots_without_waiting_for_deadline(self):
        for _ in range(140):
            with base.Peer(self.home) as peer:
                peer.send('wait','bob',{'timeout':86400})
                time.sleep(.004)
        self.wait_count(0)
        self.idle_clients()
        self.assertEqual(self.call('inbox','bob')['total'],0)

    def test_low_descriptor_limit_preserves_busy_and_short_rpc_capacity(self):
        self.call('shutdown');self.server.wait(timeout=3)
        def restrict_descriptors():
            resource.setrlimit(resource.RLIMIT_NOFILE,(256,256))
        self.server=subprocess.Popen([str(base.BINARY),'--home',self.home,'serve'],stdout=self.log,stderr=self.log,preexec_fn=restrict_descriptors)
        deadline=time.monotonic()+4
        while time.monotonic()<deadline:
            try:
                state=self.call('ping')['capacity']
                break
            except (OSError,EOFError): time.sleep(.01)
        else: self.fail('low-limit daemon did not start')
        self.assertEqual(state['descriptor_limit'],256)
        self.assertLess(state['client_limit'],128)
        self.assertEqual(state['short_reserved'],16)
        # Finite waits must consume the same long-lived permits as indefinite waits.
        with contextlib.ExitStack() as stack:
            peers=[stack.enter_context(base.Peer(self.home)) for _ in range(state['long_limit'])]
            for peer in peers: peer.send('wait','bob',{'timeout':86400})
            self.wait_count(state['long_limit'])
            busy=subprocess.run([str(base.BINARY),'--home',self.home,'--as','bob','--json','wait','--timeout','none'],capture_output=True,text=True,timeout=3)
            self.assertEqual(busy.returncode,4,busy.stdout+busy.stderr)
            self.assertIsNone(self.server.poll(),'descriptor pressure killed the daemon')
            self.assertEqual(self.call('inbox','bob')['total'],0)
            sent=self.call('send','alice',{'to':'bob','body':'Still responsive'})
            for peer in peers: self.assertTrue(peer.receive()['ok'])
            receipt=self.call('inbox','bob')['items'][0]['receipt']
            self.assertEqual(receipt['id'],sent['card']['id'])
            self.call('ack','bob',{'receipts':[receipt]})
        self.idle_clients()
        self.assertEqual(self.call('inbox','bob')['total'],0)

    def test_finite_wait_keeps_sequential_rpc_connection_usable(self):
        with base.Peer(self.home) as peer:
            result=peer.call('wait','bob',{'timeout':1})
            self.assertTrue(result['ok'],result)
            self.assertTrue(result['data']['timed_out'])
            self.assertTrue(peer.call('ping')['ok'])
            self.assertTrue(peer.call('wait','bob',{'timeout':0})['ok'])

    def test_inbound_bytes_are_protocol_errors_for_finite_and_infinite_waits(self):
        for timeout in [None,86400]:
            with base.Peer(self.home) as peer:
                peer.send('wait','bob',{'timeout':timeout})
                self.wait_count(1)
                peer.socket.sendall(b'x')
                result=peer.receive()
                self.assertFalse(result['ok'])
                self.assertEqual(result['error']['code'],'protocol')
            self.wait_count(0)

    def test_half_close_cancels_wait_and_does_not_ack(self):
        for timeout in [None,86400]:
            with base.Peer(self.home) as peer:
                peer.send('wait','bob',{'timeout':timeout})
                self.wait_count(1)
                peer.socket.shutdown(socket.SHUT_WR)
                result=peer.receive()
                self.assertFalse(result['ok'])
                self.assertEqual(result['error']['code'],'unavailable')
            self.wait_count(0)

    def test_immediate_wait_uses_reserved_short_rpc_capacity(self):
        limit=self.call('ping')['capacity']['long_limit']
        with contextlib.ExitStack() as stack:
            peers=[stack.enter_context(base.Peer(self.home)) for _ in range(limit)]
            for peer in peers: peer.send('wait','bob',{'timeout':None})
            self.wait_count(limit)
            self.assertTrue(self.call('wait','bob',{'timeout':0})['timed_out'])
        self.wait_count(0)


if __name__=='__main__': unittest.main(verbosity=2)
