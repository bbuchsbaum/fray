#!/usr/bin/env python3
"""Connection admission/cancellation regression checks on isolated boards."""
import contextlib
import json
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

    def test_long_waits_reserve_capacity_for_send_inbox_and_ack(self):
        with contextlib.ExitStack() as stack:
            peers=[stack.enter_context(base.Peer(self.home)) for _ in range(112)]
            for peer in peers: peer.send('wait','bob',{'timeout':None})
            state=self.wait_count(112)
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
        with contextlib.ExitStack() as stack:
            peers=[stack.enter_context(base.Peer(self.home)) for _ in range(112)]
            for peer in peers: peer.send('wait','bob',{'timeout':None})
            self.wait_count(112)
            self.assertTrue(self.call('wait','bob',{'timeout':0})['timed_out'])
        self.wait_count(0)


if __name__=='__main__': unittest.main(verbosity=2)
