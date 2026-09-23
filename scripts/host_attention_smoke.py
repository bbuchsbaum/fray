#!/usr/bin/env python3
"""Opt-in, real-host acceptance with a new synthetic board. Never run by CI.

Uses the operator's existing login and ordinary host permissions. No login,
installation, permission bypass, or existing session/board changes. Output is a
small receipt report; raw host output stays in the requested evidence directory.
"""
import argparse
import fcntl
import json
import os
import pathlib
import pty
import re
import select
import signal
import socket
import subprocess
import sys
import time
import termios
import struct

if len(sys.argv) == 3 and sys.argv[1] == 'mark-idle':
    pathlib.Path(sys.argv[2]).write_text(str(time.time()))
    print('{}')
    sys.exit(0)


def stop_owned(process):
    if process is None or process.poll() is not None:
        return
    # Discover descendants while the direct child is still ours. Monitor/host
    # commands may create their own process groups; never use a broad name kill.
    tree = subprocess.check_output(['ps', '-axo', 'pid=,ppid='], text=True)
    parents = {int(p):int(parent) for p,parent in (line.split() for line in tree.splitlines())}
    owned = {process.pid}
    while True:
        children = {pid for pid,parent in parents.items() if parent in owned}
        if children <= owned:
            break
        owned |= children
    for pid in sorted(owned - {process.pid}, reverse=True):
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--host', choices=['claude','codex'], required=True)
    parser.add_argument('--binary', default='target/debug/fray')
    parser.add_argument('--out', required=True)
    options = parser.parse_args()
    binary = pathlib.Path(options.binary).resolve()
    root = pathlib.Path(options.out).resolve()
    root.mkdir(mode=0o700, parents=True, exist_ok=False)
    home = root / '.fray'
    script = pathlib.Path(__file__).resolve()
    plugin = script.parents[1] / 'integrations/claude-plugin'
    actor = 'fixture-' + options.host
    server = host = None
    master = None
    report = {'host':options.host,'passed':False,'board':str(home),'scope':'synthetic receipt only'}
    env = {**os.environ,'FRAY_HOME':str(home),'FRAY_AGENT':actor,'FRAY_BIN':str(binary),'FRAY_SELECTION':'involved'}
    env.pop('FRAY_DRIVE',None)

    def call(op, who='', args=None):
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as peer:
            peer.settimeout(3)
            peer.connect(str(home / 'bus.sock'))
            with peer.makefile('rwb',buffering=0) as stream:
                stream.write((json.dumps({'op':op,'actor':who,'args':args or {}})+'\n').encode())
                response = json.loads(stream.readline())
                if not response['ok']:
                    raise RuntimeError(response['error']['code'])
                return response['data']

    try:
        report['version'] = subprocess.check_output([options.host,'--version'],stderr=subprocess.DEVNULL,text=True).strip()
        with (root/'daemon.log').open('wb') as log:
            server = subprocess.Popen([str(binary),'--home',str(home),'serve'],stdout=log,stderr=log,start_new_session=True)
        for _ in range(200):
            try:
                call('ping')
                break
            except (OSError,ValueError):
                time.sleep(.02)
        call('join','fixture-sender',{'topics':[]})
        call('join',actor,{'topics':[]})
        report['daemon_pid'] = server.pid
        prompt = (f'This is a synthetic Fray integration test in {root}. '
                  f'Use only {binary} --home {home} --as {actor} commands on this board. '
                  'Handle each presented receipt by acknowledging its exact receipt object with '
                  '`ack --receipts JSON`. If the sandbox blocks the fixture Unix socket, request scoped access through the host approval flow. Do not send replies, create work, read other files, or spawn agents. ')
        if options.host == 'claude':
            idle = root/'idle'
            settings = root/'settings.json'
            # Only the test's Stop observation hook; the plugin owns wakeup.
            import shlex
            settings.write_text(json.dumps({'hooks':{'Stop':[{'hooks':[{'type':'command','command':shlex.join([sys.executable,str(script),'mark-idle',str(idle)])}]}]}}))
            prompt += 'For this initial turn, say READY and stop. Later, handle Fray monitor notifications and stop again.'
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 120, 0, 0))
            def controlling_terminal():
                os.setsid()
                fcntl.ioctl(0, termios.TIOCSCTTY, 0)
            cmd = ['claude','--setting-sources','','--settings',str(settings),'--strict-mcp-config','--mcp-config','{"mcpServers":{}}','--no-chrome','--plugin-dir',str(plugin),'--tools','Bash','--allowedTools',f'Bash({binary} --home {home} *)','--permission-mode','manual','--',prompt]
            host = subprocess.Popen(cmd,cwd=root,env=env,stdin=slave,stdout=slave,stderr=slave,preexec_fn=controlling_terminal)
            os.close(slave)
            report['host_pid'] = host.pid
            deadline = time.monotonic()+120
            transcript = b''
            sent = None
            trusted = False
            confirm_trust_at = None
            with (root/'host.log').open('wb') as log:
                while time.monotonic()<deadline:
                    readable,_,_ = select.select([master],[],[],.2)
                    if readable:
                        try:
                            data = os.read(master,65536)
                        except OSError:
                            break
                        transcript += data
                        log.write(data)
                        log.flush()
                        # Consent only for this newly created, synthetic test folder.
                        visible = re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', transcript)
                        if not trusted and b'Yes,Itrustthisfolder' in re.sub(rb'\s+', b'', visible):
                            os.write(master,b'\x1b[B')
                            confirm_trust_at = time.monotonic()+.5
                            trusted = True
                    if confirm_trust_at is not None and time.monotonic()>=confirm_trust_at:
                        os.write(master,b'\r')
                        confirm_trust_at = None
                    if host.poll() is not None:
                        break
                    row = next(a for a in call('agents')['items'] if a['name']==actor)
                    if sent is None and idle.exists() and (row.get('listener') or {}).get('live'):
                        report['idle_before_message'] = True
                        sent = call('send','fixture-sender',{'to':actor,'body':'Synthetic wake acceptance: acknowledge this exact receipt.','ask':True})
                        report['receipt'] = {'store_id':sent['store_id'],'agent':actor,'id':sent['card']['id'],'through_seq':sent['event_seq']}
                    if sent and call('receipt_status',actor,{'receipts':[report['receipt']]})['handled']==1:
                        report['passed']=True
                        break
            if not report['passed']:
                report['blocked'] = 'Host did not reach idle with an armed monitor and acknowledge a subsequent receipt within 120 seconds; inspect host.log locally.'
        else:
            sent = call('send','fixture-sender',{'to':actor,'body':prompt,'ask':True})
            report['receipt']={'store_id':sent['store_id'],'agent':actor,'id':sent['card']['id'],'through_seq':sent['event_seq']}
            # Rules in this new fixture only. No global configuration changes.
            (root/'AGENTS.md').write_text(prompt)
            cmd=[str(binary),'--home',str(home),'--as',actor,'drive','--max-turns','1','--child-timeout','100','--','codex','exec','--ignore-user-config','--ephemeral','--skip-git-repo-check','--approve-for-me','--json','-']
            with (root/'host.log').open('wb') as log:
                host=subprocess.Popen(cmd,cwd=root,env=env,stdout=log,stderr=log,start_new_session=True)
                report['host_pid']=host.pid
                report['exit_code']=host.wait(timeout=110)
            report['passed']=report['exit_code']==0 and call('receipt_status',actor,{'receipts':[report['receipt']]})['handled']==1
            if not report['passed']:
                report['blocked']='Codex drive did not acknowledge the synthetic receipt; inspect host.log locally.'
    except (OSError,RuntimeError,ValueError,subprocess.SubprocessError) as error:
        report['blocked']=type(error).__name__ + ': ' + str(error)
    finally:
        stop_owned(host)
        if master is not None:
            os.close(master)
        stop_owned(server)
        report['owned_processes_stopped']=True
        (root/'report.json').write_text(json.dumps(report,indent=2)+'\n')
        print(json.dumps(report,indent=2))
    return 0 if report['passed'] else 1


if __name__=='__main__':
    sys.exit(main())
