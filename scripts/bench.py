#!/usr/bin/env python3
"""Measure a built Fray binary on local disk. Reports observations, not targets.
Includes Python transport/scheduling overhead. Does not measure LLM attention.
"""
import argparse, json, os, pathlib, platform, socket, statistics, subprocess, tempfile, threading, time
# Keep scratch daemons out of the user's daemon registry.
_STATE_DIR = tempfile.TemporaryDirectory(prefix="fray-state-")
os.environ["FRAY_STATE_DIR"] = _STATE_DIR.name

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary',nargs='?',default='target/release/fray')
parser.add_argument('--n',type=int,default=500)
parser.add_argument('--home-parent',default='/tmp',help='local disk directory used for the temporary store')
args=parser.parse_args();binary=str(pathlib.Path(args.binary).resolve())
if not pathlib.Path(binary).is_file(): parser.error('build with cargo build --release first')
if not 10<=args.n<=100000: parser.error('--n must be in 10..100000')

class Peer:
    def __init__(self,home):
        self.s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
        self.s.settimeout(15)
        try:
            self.s.connect(home+'/bus.sock')
        except OSError:
            self.s.close()
            raise
        self.f=self.s.makefile('rwb',buffering=0)
    def send(self,op,actor='',data=None,key=None):
        r={'op':op,'actor':actor,'args':data or {}}
        if key:r['key']=key
        self.f.write((json.dumps(r)+'\n').encode())
    def get(self):
        r=json.loads(self.f.readline())
        if not r.get('ok'):raise RuntimeError(r)
        return r['data']
    def call(self,*args,**kwargs):self.send(*args,**kwargs);return self.get()
    def close(self):self.f.close();self.s.close()

def distribution(ns):
    xs=sorted(x/1e6 for x in ns)
    def p(q):return xs[min(len(xs)-1,int((len(xs)-1)*q))]
    return {'n':len(xs),'unit':'ms','p50':p(.50),'p95':p(.95),'p99':p(.99),'max':max(xs)}

with tempfile.TemporaryDirectory(prefix='fray-bench-',dir=args.home_parent) as home:
    with open(home+'/server.log','w') as log:
        server=subprocess.Popen([binary,'--home',home,'serve'],stdout=log,stderr=log)
        p=None;w=None
        try:
            for _ in range(200):
                try:p=Peer(home);p.call('ping');break
                except OSError:time.sleep(.025)
            if p is None:raise RuntimeError('server startup failed')
            p.call('join','publisher');p.call('join','subscriber')
            w=Peer(home);w.send('watch',data={'after':0});w.get()
            sent={};observed=[];observed_lock=threading.Condition();failures=[]
            def receive():
                try:
                    while len(observed)<args.n:
                        item=w.get()
                        if item.get('type')=='event':
                            t=time.perf_counter_ns();seq=item['event']['seq']
                            with observed_lock:
                                observed.append(t-sent[seq]);observed_lock.notify_all()
                except Exception as e:failures.append(repr(e))
            thread=threading.Thread(target=receive,daemon=True);thread.start()
            rtt=[];whole=time.perf_counter_ns()
            for i in range(1,args.n+1):
                start=time.perf_counter_ns();sent[i]=start
                p.call('post','publisher',{'kind':'note','title':f'benchmark {i}','summary':'x'*160},f'bench-{i}')
                rtt.append(time.perf_counter_ns()-start)
            wall=time.perf_counter_ns()-whole
            thread.join(timeout=20)
            if len(observed)!=args.n or failures:raise RuntimeError({'delivered':len(observed),'expected':args.n,'errors':failures})
            cli=[]
            for _ in range(25):
                start=time.perf_counter_ns();subprocess.run([binary,'--home',home,'--json','ping'],check=True,stdout=subprocess.DEVNULL);cli.append(time.perf_counter_ns()-start)
            report={'environment':{'platform':platform.platform(),'python':platform.python_version(),'cpu_count':os.cpu_count(),'store_parent':args.home_parent},
                    'durability':'WAL + synchronous FULL','subscribers':1,
                    'persistent_publish_roundtrip':distribution(rtt),'publish_start_to_subscriber_read':distribution(observed),
                    'separate_process_cli_ping':distribution(cli),'serial_publishes_per_second':args.n/(wall/1e9),
                    'warning':'Includes Python framing and scheduling. One publisher, one watcher, fresh database. No agent/model latency or multi-agent scale claim.'}
            print(json.dumps(report,indent=2))
        finally:
            if p:
                try:p.call('shutdown')
                except Exception:pass
                p.close()
            if w:w.close()
            try:server.wait(timeout=3)
            except subprocess.TimeoutExpired:server.kill();server.wait()
