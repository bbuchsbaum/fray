"""Admission-ordered public feed compatibility court, using a synthetic CLI."""
import json, os, pathlib, sqlite3, subprocess, sys, tempfile, time
FRAY=str(pathlib.Path(sys.argv[1]).resolve());checks=[]
STUB=r'''#!/usr/bin/env python3
import json,os,pathlib,sys,time
s=json.loads(pathlib.Path(os.environ["FEED_STATE"]).read_text());a=sys.argv[1:]
for f in ("--store","--actor"):
 if f in a:i=a.index(f);del a[i:i+2]
if "--json" in a:a.remove("--json")
def out(v):print(json.dumps(v));sys.exit(0)
if a==["--version"]:print("mote 0.1.0");sys.exit(0)
if a==["authority","status"]:out({"schema":"mote.authority-status.v1","store_id":"st-feed","enabled":True,"authority_version":1,"genesis_digest":"fixed","capabilities":["stable_claim_order","holder_checked_handoff","checked_landing_results"]})
if a[0]=="events":
 if s.get("timeout"):time.sleep(30)
 kinds=a[a.index("--kind")+1].split(",") if "--kind" in a else []
 events=s["events"]
 if "--after" in a:
  after=a[a.index("--after")+1];ids=[e["event_id"] for e in events];events=events[ids.index(after)+1:] if after in ids else []
 for e in events:
  if not kinds or any(e["type"]==k or e["type"].startswith(k+".") for k in kinds):print(json.dumps(e))
 sys.exit(0)
if a[0]=="board":out({"active_claims":[{"id":"work","claimed_by":s["holder"],"lease_until_ts":"2030-01-01T00:00:00Z"}] if s.get("holder") else [],"active_reservations":[],"orphaned_reservations":[]})
if a[:2] in (["candidate","list"],["msg","requests"],["actor","list"]):out([])
print("unexpected feed command",a,file=sys.stderr);sys.exit(3)
'''
def raw(oid,to):return {"event_id":oid,"op_id":oid,"type":"claim.acquired","actor":to,"ts":"2026-01-01T00:00:00Z","data":{"entity":"work","to":to}}
with tempfile.TemporaryDirectory(prefix="fray-feed-",dir="/tmp") as tmp:
 root=pathlib.Path(tmp);home=root/"board";store=root/".mote";store.mkdir();(store/"FORMAT.json").write_text(json.dumps({"store_id":"st-feed"}))
 stub=root/"mote";stub.write_text(STUB);stub.chmod(0o755);state_path=root/"feed.json"
 env=os.environ.copy()
 for k in ("FRAY_HOME","FRAY_AGENT","FRAY_SESSION","MOTE_ACTOR","MOTE_SESSION","CLAUDE_SESSION_ID","CODEX_THREAD_ID"):env.pop(k,None)
 env.update(FRAY_MOTE_BIN=str(stub),MOTE_STORE=str(store),FEED_STATE=str(state_path))
 state={"events":[],"holder":None}
 def save():state_path.write_text(json.dumps(state))
 save()
 def fray(actor,*args,success=True,extra=None):
  out=subprocess.run([FRAY,"--home",str(home),"--as",actor,"--json",*args],cwd=root,env=env|(extra or {}),capture_output=True,text=True,timeout=20)
  assert (out.returncode==0)==success,(args,out.stdout,out.stderr)
  return json.loads(out.stdout)
 def sync(**kw):return fray("alice","mote","sync",**kw)
 def meta():
  with sqlite3.connect(home/"state.db") as db:return dict(db.execute("SELECT key,value FROM meta WHERE key LIKE 'mote_%'"))
 fray("","init");log=open(root/"daemon.log","w");server=subprocess.Popen([FRAY,"--home",str(home),"serve"],cwd=root,env=env,stdout=log,stderr=log)
 try:
  deadline=time.monotonic()+10
  while not (home/"bus.sock").exists():assert server.poll() is None;assert time.monotonic()<deadline;time.sleep(.01)
  for a in ("alice","bob"):fray(a,"join")
  seeded=sync();assert seeded["mote_sync"]["history_suppressed"] and meta()["mote_cursor_initialized"]=="true" and "mote_cursor" not in meta(),seeded
  state["events"]=[raw("20990101T000000.000000Z-future","alice")];state["holder"]="alice";save()
  first=sync();assert first["mote_sync"]["events"]==1 and meta().get("mote_cursor")==state["events"][0]["op_id"],(first,meta())
  checks.append("empty admitted seed has a nullable initialized anchor; first claim is read")
  projection={"event_id":"projection-expiry","op_id":"old-reserve","type":"reservation.expiring","actor":"alice","ts":"2026-01-01T00:00:00Z","data":{"holder":"alice","reservation_id":"rv-test","entity":"work","paths":["src/a"],"deadline":"2026-12-01T00:00:00Z"}}
  state["events"].append(projection);save();warn=sync();assert warn["mote_sync"]["created"] and meta()["mote_cursor"]==state["events"][0]["op_id"],warn
  quiet=sync();assert not quiet["mote_sync"]["created"],quiet
  checks.append("due projection behind future raw cursor reaches holder once without advancing anchor")
  state["events"].append(raw("20000101T000000.000000Z-late","bob"));state["holder"]="bob";save()
  moved=sync();assert moved["mote_sync"]["events"]==2 and meta()["mote_cursor"]==state["events"][-1]["op_id"],moved
  assert not sync()["mote_sync"]["created"]
  checks.append("later admitted earlier-spelled claim advances feed anchor and remains quiet on retry")
  before=meta();state["timeout"]=True;save()
  for _ in range(3):
   failed=sync(success=False,extra={"FRAY_MOTE_READ_TIMEOUT_MS":"200"});assert "no UTC reseed" in failed["error"]["message"],failed
  assert meta()==before
  checks.append("three admitted-feed timeouts preserve exact cursor and revision")
  state["timeout"]=False;save()
  # A fresh second board adopts a populated history without replaying its handoffs.
  old_home=home;home=root/"second";fray("","init")
  second_log=open(root/"second.log","w");second=subprocess.Popen([FRAY,"--home",str(home),"serve"],cwd=root,env=env,stdout=second_log,stderr=second_log)
  try:
   deadline=time.monotonic()+10
   while not (home/"bus.sock").exists():assert second.poll() is None;assert time.monotonic()<deadline;time.sleep(.01)
   for a in ("alice","bob"):fray(a,"join")
   populated=sync();assert populated["mote_sync"]["history_suppressed"] and populated["mote_sync"]["events"]==0,populated
   assert not any("you now hold" in i["card"]["title"] or "now held" in i["card"]["title"] for i in fray("bob","inbox","--selection","all")["items"])
   assert not sync()["mote_sync"]["created"]
   checks.append("populated admitted seed suppresses old handoffs and unchanged sync stays quiet")
  finally:
   if second.poll() is None:
    try:fray("","stop");second.wait(timeout=10)
    except Exception:second.terminate();second.wait(timeout=10)
   second_log.close();home=old_home
 finally:
  if server.poll() is None:
   try:fray("","stop");server.wait(timeout=10)
   except Exception:server.terminate();server.wait(timeout=10)
  log.close()
print(json.dumps({"passed":len(checks),"checks":checks,"synthetic_public_cli":True,"paid_trials":False,"shared_stores_touched":False}))
