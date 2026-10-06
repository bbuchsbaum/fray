import json, pathlib, shutil, socket, subprocess, tempfile, time

binary = "/private/tmp/fray-qualification-9be4977/fray"
profile = "/private/tmp/fray-seatbelt-9be4977.sb"
home = tempfile.mkdtemp(prefix="fray-seatbelt-", dir="/private/tmp")
log = pathlib.Path("/private/tmp/fray-seatbelt-9be4977.log")
daemon = subprocess.Popen(["/usr/bin/sandbox-exec", "-f", profile, binary, "--home", home, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
result = {"binary": binary, "profile": profile, "home": home, "pid": daemon.pid}
try:
    deadline = time.monotonic() + 8
    sock_path = pathlib.Path(home) / "bus.sock"
    while time.monotonic() < deadline:
        if sock_path.exists(): break
        if daemon.poll() is not None: raise RuntimeError("daemon exited: " + daemon.stderr.read())
        time.sleep(.02)
    else: raise RuntimeError("daemon did not create socket")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(5); sock.connect(str(sock_path)); stream = sock.makefile("rwb", buffering=0)
        for request in [
            {"op":"ping","actor":"","args":{}},
            {"op":"keepalive_start","actor":"synthetic","args":{"cwd":"/private/tmp"}},
        ]:
            stream.write((json.dumps(request) + "\n").encode())
            result[request["op"]] = json.loads(stream.readline())
    result["host_spawned"] = False
finally:
    if daemon.poll() is None:
        daemon.terminate()
        try: daemon.wait(timeout=3)
        except subprocess.TimeoutExpired: daemon.kill(); daemon.wait()
    result["daemon_exit"] = daemon.returncode
    result["socket_exists_after_cleanup"] = (pathlib.Path(home) / "bus.sock").exists()
    shutil.rmtree(home, ignore_errors=True)
    result["home_removed"] = not pathlib.Path(home).exists()
    log.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(json.dumps(result, sort_keys=True))
