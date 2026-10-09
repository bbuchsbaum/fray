"""Synthetic public-CLI recovery court; no installed Mote, models or shared stores."""
import json, os, pathlib, sqlite3, subprocess, sys, tempfile, time
# Keep scratch daemons out of the user's daemon registry.
_STATE_DIR = tempfile.TemporaryDirectory(prefix="fray-state-")
os.environ["FRAY_STATE_DIR"] = _STATE_DIR.name
FRAY = str(pathlib.Path(sys.argv[1]).resolve())
checks = []
STUB = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
p=pathlib.Path(os.environ["FAKE_STATE"]);s=json.loads(p.read_text());a=sys.argv[1:]
actor=a[a.index("--actor")+1] if "--actor" in a else ""
for flag in ("--store","--actor"):
 if flag in a:
  i=a.index(flag);del a[i:i+2]
if "--json" in a:a.remove("--json")
def out(v):print(json.dumps(v));p.write_text(json.dumps(s));sys.exit(0)
if a==["--version"]:print("mote 0.1.0");sys.exit(0)
if a==["authority","status"]:out({"schema":"mote.authority-status.v1","store_id":"st-fake","enabled":True,"authority_version":1,"genesis_digest":"fixed","capabilities":["stable_claim_order","holder_checked_handoff","checked_landing_results"]})
if a[0]=="show":out({"id":a[1],"status":"open"})
if a[0]=="history":out(s["histories"].get(a[1],[]))
if a[0]=="board":
 board={"active_claims":[{"id":i,"claimed_by":c["holder"],"lease_until_ts":c["lease"]} for i,c in s["claims"].items()],"active_reservations":s.get("reservations",[]),"orphaned_reservations":[]}
 if s.get("race"):
  s["claims"]["race"]={"holder":"bob","token":"B","lease":"2030-01-01T00:00:00Z"};s["histories"]["race"].append({"accepted":True,"kind":"handoff","op_id":"B"});s["race"]=False
 if s.get("churn"):
  n=len(s["histories"]["race"]);s["histories"]["race"].append({"accepted":True,"kind":"claim","op_id":str(n)})
 out(board)
if a[0]=="claim":
 issue=a[1];token="acquired-"+actor;s["claims"][issue]={"holder":actor,"token":token,"lease":"2030-01-01T00:00:00Z"};s["histories"].setdefault(issue,[]).append({"accepted":True,"kind":"claim","op_id":token});out(None)
print("unexpected fake command",a,file=sys.stderr);sys.exit(3)
'''
with tempfile.TemporaryDirectory(prefix="fray-dc-",dir="/tmp") as tmp:
    root=pathlib.Path(tmp);home=root/"board";store=root/".mote";store.mkdir()
    (store/"FORMAT.json").write_text(json.dumps({"store_id":"st-fake"}))
    stub=root/"mote";stub.write_text(STUB);stub.chmod(0o755)
    state_path=root/"state.json";state={"claims":{},"histories":{}}
    def save():state_path.write_text(json.dumps(state))
    save();env=os.environ.copy()
    for k in ("FRAY_HOME","FRAY_AGENT","FRAY_SESSION","MOTE_ACTOR","MOTE_SESSION","CLAUDE_SESSION_ID","CODEX_THREAD_ID"):env.pop(k,None)
    env.update(FRAY_MOTE_BIN=str(stub),MOTE_STORE=str(store),FAKE_STATE=str(state_path),FRAY_SESSION="codex:transport-first")
    def fray(actor,*args,success=True):
        out=subprocess.run([FRAY,"--home",str(home),"--as",actor,"--json",*args],cwd=root,env=env,capture_output=True,text=True,timeout=20)
        assert (out.returncode==0)==success,(args,out.stdout,out.stderr)
        return json.loads(out.stdout)
    fray("","init");log=open(root/"daemon.log","w");server=None
    def start():
        global server
        server=subprocess.Popen([FRAY,"--home",str(home),"serve"],cwd=root,env=env,stdout=log,stderr=log)
        deadline=time.monotonic()+10
        while not (home/"bus.sock").exists():
            assert server.poll() is None,(root/"daemon.log").read_text()
            assert time.monotonic()<deadline;time.sleep(.01)
    def stop():
        if server and server.poll() is None:fray("","stop");server.wait(timeout=10)
    try:
        start()
        for actor in ("writer","alice","bob"):fray(actor,"join","--topics","rust");fray(actor,"status","idle")
        state={"claims":{"race":{"holder":"alice","token":"A","lease":"2030-01-01T00:00:00Z"}},"histories":{"race":[{"accepted":True,"kind":"claim","op_id":"A"}]},"race":True};save()
        source=fray("alice","send","bob","Race work","--ref","mote:race")["card"]["id"]
        denied=fray("alice","--key","mixed","handoff",str(source),"--to","bob","--state","Half","--next","Finish",success=False)
        assert denied["error"]["code"]=="mote_not_holder",denied
        checks.append("transfer between history/board reads retries to the actual holder")
        state=json.loads(state_path.read_text());state["churn"]=True;save()
        denied=fray("alice","--key","churn","handoff",str(source),"--to","bob","--state","Half","--next","Finish",success=False)
        assert denied["error"]["code"]=="mote_unconfirmed",denied
        checks.append("continuous history churn refuses after bounded reads")
        state={"claims":{},"histories":{}};save()
        offered=fray("writer","--key","offer","send","anyone-free","Work","--mote","work","--tag","rust")
        card=str(offered["result"]["offer"]["offer"]["id"])
        assert fray("alice","--key","accept","accept",card)["state"]=="completed"
        # Offline fixture of crash after confirmation commit, before finalize.
        stop()
        with sqlite3.connect(home/"state.db") as db:db.execute("UPDATE mote_operations SET state='pending',result=NULL WHERE actor='alice' AND key='accept'")
        state=json.loads(state_path.read_text());state["claims"]["work"]["token"]="renewed";state["histories"]["work"].append({"accepted":True,"kind":"claim","op_id":"renewed"});save()
        start();env["FRAY_SESSION"]="codex:transport-second";fray("alice","join","--takeover")
        recovered=fray("alice","operation","resume","accept")
        assert recovered["state"]=="completed" and recovered["result"]["historical_receipt"],recovered
        assert recovered["result"]["claim_observation"]["token"]=="acquired-alice" and recovered["result"]["current_claim"]["token"]=="renewed",recovered
        checks.append("committed confirmation plus lost finalization/renewal/new host session recovers exact saved response")
        stop()
        with sqlite3.connect(home/"state.db") as db:
            db.execute("UPDATE mote_operations SET state='pending',result=NULL WHERE actor='alice' AND key='accept'")
            db.execute("DELETE FROM requests WHERE actor='alice' AND json_extract(request,'$.op')='dispatch_status'")
        start();env["FRAY_SESSION"]="codex:transport-third";fray("alice","join","--takeover")
        refused=fray("alice","operation","resume","accept",success=False)
        assert refused["error"]["code"]=="dispatch_session_ended",refused
        assert fray("alice","operation","show","accept")["state"]=="pending"
        assert len(json.loads(state_path.read_text())["histories"]["work"])==2
        checks.append("no committed response plus ended attempt refuses without old-session replay or claim reacquisition")
        state["reservations"]=[{"reservation_id":"rv-self","actor":"alice","entity":"work","paths":["src/a"],"lease_until_ts":"2030-01-01T00:00:00Z"}];save()
        denied=fray("alice","--key","self-carrier","handoff",card,"--to","bob","--state","Half","--next","Finish","--carrier","work=rv-self",success=False)
        assert denied["error"]["code"]=="invalid" and "must differ" in denied["error"]["message"],denied
        checks.append("work issue cannot be its own closable carrier")
    finally:
        if server and server.poll() is None:
            try:stop()
            except Exception:server.terminate();server.wait(timeout=10)
        log.close()
print(json.dumps({"passed":len(checks),"checks":checks,"synthetic_public_cli":True,"paid_trials":False,"shared_stores_touched":False}))
