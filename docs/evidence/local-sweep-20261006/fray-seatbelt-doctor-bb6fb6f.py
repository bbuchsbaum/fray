import hashlib, json, pathlib, shutil, socket, subprocess, tempfile, time

binary = "/private/tmp/fray-qualification-bb6fb6f/fray"
profile = "/private/tmp/fray-seatbelt-9be4977.sb"
home = tempfile.mkdtemp(prefix="fray sb's ", dir="/private/tmp")
log = pathlib.Path("/private/tmp/fray-seatbelt-doctor-bb6fb6f.log")
sha = hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest()
daemon = subprocess.Popen(["/usr/bin/sandbox-exec", "-f", profile, binary, "--home", home, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
result = {"artifact": binary, "sha256": sha, "profile": profile, "profile_scope": "custom real seatbelt denies process-exec of nested sandbox-exec; release probe refusal only, not Codex inheritance or general nested-seatbelt behavior", "home": home, "identity": "synthetic", "pid": daemon.pid, "native_host_invoked": False}
try:
    deadline=time.monotonic()+8
    while time.monotonic()<deadline:
        if (pathlib.Path(home)/"bus.sock").exists(): break
        if daemon.poll() is not None: raise RuntimeError(daemon.stderr.read())
        time.sleep(.02)
    else: raise RuntimeError("no socket")
    with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as sock:
        sock.connect(str(pathlib.Path(home)/"bus.sock")); f=sock.makefile("rwb",buffering=0)
        f.write(b'{"op":"join","actor":"synthetic","args":{}}\n'); result["join"]=json.loads(f.readline())
        f.write((json.dumps({"op":"keepalive_start","actor":"synthetic","args":{"cwd":"/private/tmp"}})+"\n").encode()); result["start"]=json.loads(f.readline())
    doctor=subprocess.run([binary,"--home",home,"--as","synthetic","--json","doctor"],text=True,capture_output=True,timeout=8)
    result["doctor_exit"]=doctor.returncode; result["doctor"]=json.loads(doctor.stdout)
    result["daemon_sandboxed"]=result["doctor"]["keepalive"]["daemon_sandboxed"]
    result["recovery"]=[x["message"] for x in result["doctor"]["checks"] if x["code"]=="keepalive_daemon_sandboxed"]
finally:
    if daemon.poll() is None:
        daemon.terminate()
        try: daemon.wait(timeout=3)
        except subprocess.TimeoutExpired: daemon.kill();daemon.wait()
    result["daemon_exit"]=daemon.returncode; result["socket_existed_before_cleanup"]=(pathlib.Path(home)/"bus.sock").exists()
    shutil.rmtree(home,ignore_errors=True);result["home_removed"]=not pathlib.Path(home).exists()
    log.write_text(json.dumps(result,indent=2,sort_keys=True)+"\n")
    print(json.dumps(result,sort_keys=True))
