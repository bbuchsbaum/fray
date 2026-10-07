"""Actual CLI dispatch/handoff court using disposable stores and no models."""
import json,os,pathlib,subprocess,sys,tempfile,time
FRAY,MOTE=[str(pathlib.Path(p).resolve()) for p in sys.argv[1:3]]
checks=[]
with tempfile.TemporaryDirectory(prefix="fray-dispatch-court-",dir="/tmp") as td:
 root=pathlib.Path(td);home=root/"board";store=root/".mote";env=os.environ.copy()
 for k in ("FRAY_HOME","FRAY_AGENT","FRAY_SESSION","MOTE_STORE","MOTE_ACTOR","MOTE_SESSION","CLAUDE_SESSION_ID","CODEX_THREAD_ID"):env.pop(k,None)
 env.update(MOTE_STORE=str(store),FRAY_MOTE_BIN=MOTE)
 def raw(argv,extra=None):return subprocess.run(argv,cwd=root,env=env|(extra or {}),capture_output=True,text=True,timeout=45)
 def mote(actor,*argv):
  out=raw([MOTE,"--store",str(store),"--actor",actor,"--json",*argv]);assert out.returncode==0,(argv,out.stdout,out.stderr)
  try:return json.loads(out.stdout)
  except ValueError:return out.stdout.strip()
 def fray(actor,*argv,success=True,extra=None):
  out=raw([FRAY,"--home",str(home),"--as",actor,"--json",*argv],extra)
  if success:assert out.returncode==0,(argv,out.stdout,out.stderr)
  else:assert out.returncode!=0,(argv,out.stdout,out.stderr)
  return json.loads(out.stdout)
 mote("writer","init");mote("writer","authority","enable");mote("writer","new","--id","work","Concurrent work")
 fray("","init");log=open(root/"daemon.log","w");server=subprocess.Popen([FRAY,"--home",str(home),"serve"],cwd=root,env=env,stdout=log,stderr=log)
 try:
  deadline=time.monotonic()+10
  while not (home/"bus.sock").exists():assert server.poll() is None;assert time.monotonic()<deadline;time.sleep(.02)
  for actor in ("writer","alice","bob"):fray(actor,"join","--topics","rust");fray(actor,"status","idle")
  offer=fray("writer","--key","offer","send","--to","anyone-free","Bounded work","--mote","work","--tag","rust","--claim-ttl","300")
  card=str(offer["result"]["offer"]["offer"]["id"])
  assert offer["result"]["offer"]["offer"]["offered_to"]=="alice"
  children=[]
  for actor in ("alice","bob"):
   argv=[FRAY,"--home",str(home),"--as",actor,"--json","--key",actor+"-accept","accept",card,"--expect","1"]
   children.append((actor,subprocess.Popen(argv,cwd=root,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)))
  results=[]
  for actor,child in children:
   stdout,stderr=child.communicate(timeout=30);results.append((actor,child.returncode,json.loads(stdout),stderr))
  winners=[r for r in results if r[1]==0];assert len(winners)==1,results
  winner=winners[0][0];recipient="bob" if winner=="alice" else "alice"
  board=mote("writer","board");claim=[c for c in board["active_claims"] if c["id"]=="work"];assert len(claim)==1 and claim[0]["claimed_by"]==winner
  assert len([e for e in mote("writer","history","work") if e["kind"]=="claim" and e["accepted"]])==1
  checks.append("two concurrent explicit accepts produce exactly one current Mote winner and one claim publication")
  # Carrier stays live under the sender through transfer and close/adopt.
  mote(winner,"new","--id","carrier","Carrier");mote(winner,"claim","carrier","--ttl","600")
  rv=mote(winner,"reserve","--issue","carrier","--ttl","600","src/work.rs")["reservation_id"]
  failed=fray(winner,"--key","handoff","handoff",card,"--to",recipient,"--state","Half done","--next","Finish","--carrier","carrier="+rv,"--reservation-ttl","600",success=False,extra={"MOTE_TEST_AUTHORITY_FAIL":"publication-admitted"})
  assert failed["error"]["code"]=="mote_unconfirmed"
  current=mote(winner,"board");assert [r for r in current["active_reservations"] if r["reservation_id"]==rv][0]["actor"]==winner
  transferred=fray(winner,"handoff","--resume","handoff")
  packet=str(transferred["result"]["packet"]["id"])
  assert transferred["state"]=="completed"
  checks.append("post-admission handoff failure remains pending; exact-key resume confirms transfer while sender carrier stays reserved")
  # Interrupt closure before publication. Mote's publication journal and
  # Fray's exact request are recovered on the recipient's next resume.
  first=fray(recipient,"--key","adopt","accept",packet,success=False,extra={"MOTE_TEST_AUTHORITY_FAIL":"publication-prepared"})
  assert first["error"]["code"]=="mote_unconfirmed"
  held=mote(recipient,"who-has","src/work.rs");assert any(r["reservation_id"]==rv for r in held)
  adopted=fray(recipient,"operation","resume","adopt")
  assert adopted["state"]=="completed"
  held=mote(recipient,"who-has","src/work.rs");assert any(r["reservation_id"]==rv and r["actor"]==recipient and r["entity"]=="work" for r in held)
  lease=held[0]["lease_until_ts"]
  again=fray(recipient,"operation","resume","adopt");assert again["state"]=="completed"
  assert mote(recipient,"who-has","src/work.rs")[0]["lease_until_ts"]==lease
  checks.append("interrupted carrier close/adopt resumes with no observed unreserved interval; repeated adoption resume does not renew")
  # A public-CLI wrapper admits a competing adoption after carrier closure,
  # or lets its live TTL expire. It touches only this disposable fixture.
  for loss in ("competitor","expiry"):
   work="loss-"+loss;carrier="carrier-"+loss;rival="rival-"+loss
   mote(winner,"new","--id",work,"Loss court");mote(winner,"claim",work,"--ttl","600")
   source=fray(winner,"send",recipient,"Handoff failure court","--ref","mote:"+work)["card"]["id"]
   mote(winner,"new","--id",carrier,"Carrier");mote(winner,"claim",carrier,"--ttl","600")
   reservation=mote(winner,"reserve","--issue",carrier,"--ttl","2" if loss=="expiry" else "600","src/"+loss)["reservation_id"]
   mote("competitor","new","--id",rival,"Competing work");mote("competitor","claim",rival,"--ttl","600")
   transferred=fray(winner,"--key",loss,"handoff",str(source),"--to",recipient,"--state","Partial","--next","Finish","--carrier",carrier+"="+reservation)
   packet_id=str(transferred["result"]["packet"]["id"])
   wrapper=root/("mote-"+loss)
   wrapper.write_text("#!/usr/bin/env python3\nimport json,subprocess,sys,time\nreal="+repr(MOTE)+"\na=sys.argv[1:]\np=subprocess.run([real,*a],capture_output=True)\nsys.stdout.buffer.write(p.stdout);sys.stderr.buffer.write(p.stderr)\nif 'close' in a and "+repr(carrier)+" in a and p.returncode==0:\n base=[real,'--store',"+repr(str(store))+",'--actor','competitor','--json']\n def board():return json.loads(subprocess.check_output([*base,'board']))\n rv="+repr(reservation)+"\n if "+repr(loss)+"=='competitor':\n  row=next(r for r in board()['orphaned_reservations'] if r['reservation_id']==rv)\n  subprocess.run([*base,'adopt',rv,'--issue',"+repr(rival)+",'--expect-reservation',row['clock'],'--ttl','600'],check=True,stdout=subprocess.DEVNULL)\n else:\n  deadline=time.monotonic()+4\n  while any(r['reservation_id']==rv for r in board()['orphaned_reservations']):\n   assert time.monotonic()<deadline;time.sleep(.03)\nsys.exit(p.returncode)\n")
   wrapper.chmod(0o755)
   failed=fray(recipient,"--key","adopt-"+loss,"accept",packet_id,success=False,extra={"FRAY_MOTE_BIN":str(wrapper)})
   assert failed["error"]["code"]=="reservation_lost",failed
   shown=fray(recipient,"show",packet_id,"--history")
   assert any(e["payload"].get("detail",{}).get("status")=="lost" for e in shown["history"]),shown
   for actor in (winner,recipient):assert any(str(i["card"]["id"])==packet_id for i in fray(actor,"inbox","--selection","all")["items"])
   if loss=="competitor":assert mote(recipient,"who-has","src/"+loss)[0]["actor"]=="competitor"
   checks.append(loss+" during carrier orphan interval reports loss to both parties and never claims held paths")
  # A separate short claim demonstrates lapsed ownership and notice/requeue.
  mote("writer","new","--id","short","Short claim")
  short=fray("writer","--key","short-offer","send","anyone-free","Short work","--mote","short","--tag","rust","--claim-ttl","2")
  sid=str(short["result"]["offer"]["offer"]["id"])
  accepted=fray("alice","--key","short-accept","accept",sid)
  fray("alice","leave")
  stalled=fray("writer","dispatch","sync");row=[r for r in stalled["offers"] if str(r["id"])==sid][0];assert row["status"]=="stalled" and row["generation"]==1
  deadline=time.monotonic()+4
  while any(c["id"]=="short" for c in mote("writer","board")["active_claims"]):assert time.monotonic()<deadline;time.sleep(.05)
  requeued=fray("writer","dispatch","sync");row=[r for r in requeued["offers"] if str(r["id"])==sid][0];assert row["generation"]==2 and row["offered_to"]=="bob"
  notice=fray("writer","inbox");assert any("disappeared" in x["card"]["title"] for x in notice["items"])
  checks.append("disappearance notifies sender immediately, preserves live Mote claim, then requeues after actual bounded expiry")
 finally:
  if server.poll() is None:
   try:fray("","stop")
   except Exception:server.terminate()
   server.wait(timeout=15)
  log.close()
print(json.dumps({"passed":len(checks),"checks":checks,"fray_binary":FRAY,"mote_binary":MOTE,"paid_trials":False,"shared_stores_touched":False},indent=2))
