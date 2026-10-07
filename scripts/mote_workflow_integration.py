"""Real, disposable Fray/Mote/Git acceptance court; no models or shared stores."""
import json, os, pathlib, subprocess, sys, tempfile, time

FRAY, MOTE = map(lambda p: str(pathlib.Path(p).resolve()), sys.argv[1:3])
checks = []
def court():
    with tempfile.TemporaryDirectory(prefix="fray-mote-court-",dir="/tmp") as tmp:
        root=pathlib.Path(tmp); home=root/"board"; store=root/".mote"
        env=os.environ.copy()
        for name in ("FRAY_HOME","FRAY_AGENT","FRAY_SESSION","MOTE_STORE","MOTE_ACTOR","CLAUDE_SESSION_ID","CODEX_THREAD_ID","MOTE_SESSION"):
            env.pop(name,None)
        env.update(FRAY_MOTE_BIN=MOTE,MOTE_STORE=str(store))
        def command(argv,extra=None,success=True):
            out=subprocess.run(argv,cwd=root,env=env| (extra or {}),capture_output=True,text=True,timeout=45)
            if success and out.returncode: raise AssertionError((argv,out.returncode,out.stdout,out.stderr))
            return out
        def git(*args): return command(["git",*args]).stdout.strip()
        def mote(actor,*args,success=True,extra=None):
            out=command([MOTE,"--store",str(store),"--actor",actor,"--json",*args],extra,success)
            if not success:return out
            try:return json.loads(out.stdout)
            except ValueError:return out.stdout.strip()
        def fray(actor,*args,success=True,extra=None):
            out=command([FRAY,"--home",str(home),"--as",actor,"--json",*args],extra,success)
            value=json.loads(out.stdout)
            if success:
                return value
            return out.returncode,value
        git("init","-q","-b","main");git("config","user.name","Court");git("config","user.email","court@example.invalid")
        (root/"work.txt").write_text("base\n");git("add","work.txt");git("commit","-qm","base")
        base=git("rev-parse","HEAD")
        mote("writer","init");mote("writer","new","--id","work","Review court")
        mote("writer","authority","enable");mote("writer","claim","work","--ttl","3600")
        git("switch","-qc","candidate");(root/"work.txt").write_text("first\n");git("commit","-qam","first")
        first=git("rev-parse","HEAD")
        def propose(key): return mote("writer","candidate","propose","--issue","work","--base",base,"--path","work.txt","--authorizer","author","--reviewer","reader","--idempotency-key",key)
        old=propose("proposal-one");old_id=old["candidate_id"]
        fray("","init")
        log=open(root/"daemon.log","w")
        server=subprocess.Popen([FRAY,"--home",str(home),"serve"],cwd=root,env=env,stdout=log,stderr=log)
        try:
            deadline=time.monotonic()+10
            while not (home/"bus.sock").exists():
                assert server.poll() is None,(root/"daemon.log").read_text()
                assert time.monotonic()<deadline
                time.sleep(.02)
            for actor in ("writer","reader","lander","author"):fray(actor,"join")
            request=fray("writer","--key","request-one","review","candidate",old_id,"--to","reader","--title","Review first","Check behavior")
            card=str(request["card"]["id"])
            unconfirmed=fray("reader","--key","object-one","review","candidate-verdict",card,"object","--at","git:"+first,"--expect","1","Missing validation",success=False,extra={"MOTE_TEST_AUTHORITY_FAIL":"publication-admitted"})
            assert unconfirmed[0]!=0 and not fray("reader","show",card)["review"]["verdicts"],unconfirmed
            objected=fray("reader","operation","resume","object-one")
            assert objected["state"]=="completed"
            checks.append("nonzero admitted Mote review leaves no Fray verdict; exact-key recovery confirms acceptance")
            assert mote("reader","candidate","show",old_id)["reviews"]["reader"]["verdict"]=="block"
            # Mutable current-view changes must not alter the saved request.
            repeated=fray("writer","--key","request-one","review","candidate",old_id,"--to","reader","--title","Review first","Check behavior")
            assert repeated["card"]["id"]==int(card)
            checks.append("request -> accepted Mote block; identical request retry deduplicates despite changed review state")
            (root/"work.txt").write_text("fixed\n");git("commit","-qam","fix")
            second=git("rev-parse","HEAD");new=propose("proposal-two");new_id=new["candidate_id"]
            mote("writer","candidate","supersede",old_id,new_id,"--expect-phase",old["phase"]["op_id"],"--idempotency-key","supersede")
            successor=fray("writer","--key","successor","review","successor",card,"--candidate",new_id,"--expect","1")
            new_card=str(successor["successor"]["card"]["id"])
            assert len(successor["carried_objections"])==1
            shown=fray("reader","show",new_card,"--history")
            assert shown["review"]["mote_candidate"]["candidate_id"]==new_id
            assert not shown["review"]["verdicts"]
            checks.append("verified successor creates immutable review, carries open objection, copies no approval")
            approved=fray("reader","--key","approve-two","review","candidate-verdict",new_card,"approve","--at","git:"+second,"--expect","1","Fixed behavior")
            assert approved["state"]=="completed"
            current=mote("reader","candidate","show",new_id)
            mote("reader","candidate","review",new_id,"block","--expect",current["reviews"]["reader"]["op_id"],"--body","Later objection","--idempotency-key","direct-block")
            retried=fray("reader","operation","resume","approve-two")
            assert retried["historical_receipt"] and retried["current_candidate"]["reviews"]["reader"]["verdict"]=="block"
            checks.append("completed approval retry reports historical receipt and preserves later authoritative Mote block")
            # A new explicit approval must compare the current Mote review token.
            approved=fray("reader","--key","approve-final","review","candidate-verdict",new_card,"approve","--at","git:"+second,"--expect","1","Final checks")
            auth=mote("author","candidate","authorize",new_id,"--grantee","lander","--idempotency-key","authorization")
            mote("author","candidate","evidence","target-scope",new_id,"--target","main","--idempotency-key","scope")
            ready=fray("lander","land",new_id,"--target","main","--check")
            assert ready["eligible"] and git("rev-parse","main")==base
            fray("writer","mote","sync")
            reader_before=len(fray("reader","inbox","--selection","all")["items"])
            git("update-ref","refs/heads/main",first,base)
            fray("writer","mote","sync")
            assert len(fray("reader","inbox","--selection","all")["items"])==reader_before
            assert any("base moved" in i["card"]["title"] for i in fray("writer","inbox","--selection","all")["items"])
            git("update-ref","refs/heads/main",base,first)
            mote("author","candidate","evidence","target-scope",new_id,"--target","main","--idempotency-key","scope-refresh")
            fray("writer","mote","sync")
            checks.append("actual main advance reports local evidence mismatch to producers without re-waking reviewer")
            denied=fray("writer","--key","self-review","review","candidate-verdict",new_card,"approve","--at","git:"+second,"--expect","1","Self approval",success=False)
            assert denied[0]!=0 and denied[1]["error"]["code"]=="self_review"
            checks.append("readonly landing check leaves Git unchanged; proposer self approval refused")
            # Revoke after check: fenced mutation must refuse and leave Git alone.
            revoked=mote("author","candidate","revoke",new_id,"--expect",auth["authorization"]["op_id"],"--idempotency-key","revoke")
            rejected=fray("lander","--key","revoked-land","land",new_id,"--target","main",success=False)
            assert rejected[0]!=0 and git("rev-parse","main")==base
            checks.append("revocation after eligibility check refuses landing without ref change")
            mote("author","candidate","authorize",new_id,"--grantee","lander","--expect",revoked["authorization"]["op_id"],"--idempotency-key","authorization-final")
            # Hold Mote at its post-update checkpoint. An unrelated Fray RPC
            # must finish while the Mote writer is blocked.
            signal=root/"pause"
            argv=[FRAY,"--home",str(home),"--as","lander","--json","--key","land-final","land",new_id,"--target","main"]
            landing=subprocess.Popen(argv,cwd=root,env=env|{"MOTE_TEST_AUTHORITY_PAUSE":"landing-updated","MOTE_TEST_AUTHORITY_SIGNAL":str(signal)},stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            try:
                deadline=time.monotonic()+15
                while not signal.exists():
                    assert landing.poll() is None
                    assert time.monotonic()<deadline
                    time.sleep(.02)
                start=time.monotonic();fray("writer","ping");elapsed=time.monotonic()-start
                assert elapsed<2,elapsed
                signal.unlink();stdout,stderr=landing.communicate(timeout=20)
                assert landing.returncode==0,(stdout,stderr)
                landed=json.loads(stdout)
                assert landed["state"]=="completed" and git("rev-parse","main")==second
                assert landed["result"]["pushed"] is False
                checks.append(f"fenced real fast-forward completed; unrelated Fray RPC during blocked Mote returned in {elapsed:.4f}s")
            finally:
                if signal.exists():signal.unlink()
                if landing.poll() is None:landing.terminate();landing.wait(timeout=10)
            again=fray("lander","operation","resume","land-final")
            assert again["historical_receipt"] and again["target_current"]
            checks.append("exact-key landing retry reads historical receipt without another Git mutation")
        finally:
            if server.poll() is None:
                try:fray("","stop")
                except Exception:server.terminate()
                server.wait(timeout=15)
            log.close()
court()
print(json.dumps({"checks":checks,"passed":len(checks),"fray_binary":FRAY,"mote_binary":MOTE,"paid_trials":False,"shared_stores_touched":False},indent=2))
