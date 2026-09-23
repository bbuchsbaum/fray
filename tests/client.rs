use fray::{
    client,
    model::*,
    server::{read_frame, write_frame, REQUEST_LIMIT},
};
use serde_json::{json, Value};
use std::{
    fs,
    io::BufReader,
    os::unix::net::UnixListener,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

struct Mock {
    home: PathBuf,
    server: Option<thread::JoinHandle<Vec<Vec<Request>>>>,
}
impl Mock {
    fn new(hellos: Vec<Value>) -> Self {
        let home = PathBuf::from("/tmp").join(format!("fray-client-{}", random_key().unwrap()));
        fs::create_dir(&home).unwrap();
        let listener = UnixListener::bind(home.join("bus.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let mut connections = Vec::new();
            for hello in hellos {
                let deadline = Instant::now() + Duration::from_secs(5);
                let stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(e) => panic!("mock accept: {e}"),
                    }
                };
                // macOS may inherit O_NONBLOCK from the listening socket.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut requests = Vec::new();
                while let Some(line) = read_frame(&mut reader, REQUEST_LIMIT).unwrap() {
                    let request: Request = serde_json::from_str(&line).unwrap();
                    let response = if request.op == "ping" {
                        hello.clone()
                    } else {
                        json!({"op":request.op,"args":request.args})
                    };
                    requests.push(request);
                    if hello.is_null() {
                        break;
                    }
                    write_frame(reader.get_mut(), &success(response)).unwrap();
                }
                connections.push(requests);
            }
            connections
        });
        Self {
            home,
            server: Some(server),
        }
    }
    fn requests(mut self) -> Vec<Vec<Request>> {
        self.server.take().unwrap().join().unwrap()
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.home).unwrap();
    }
}
fn hello(version: &str, protocol: Value) -> Value {
    json!({"version":version,"protocol_version":protocol})
}

#[test]
fn incompatible_or_missing_protocol_sends_only_ping() {
    for metadata in [
        hello("0.1.0", json!(1)),
        hello("future", json!(3)),
        json!({}),
        hello("bad", json!("2")),
        hello("bad", json!(null)),
    ] {
        let mock = Mock::new(vec![metadata]);
        let request = Request::new("post", "fixture", json!({"title":"must not be sent"}));
        let error = client::rpc(&mock.home, &request, 5).unwrap_err();
        assert_eq!(error.code, "protocol_version");
        assert!(error.message.contains(env!("CARGO_PKG_VERSION")));
        assert!(error.message.contains("No operational request sent"));
        let requests = mock.requests();
        assert_eq!(requests[0].len(), 1);
        assert_eq!(requests[0][0].op, "ping");
        assert!(requests[0][0].actor.is_empty());
    }
}

#[test]
fn matching_protocol_ignores_package_version_and_preserves_request_on_same_socket() {
    let mock = Mock::new(vec![hello("different-package-version", json!(2))]);
    let mut request = Request::new(
        "inbox",
        "fixture",
        json!({"selection":"involved","limit":4}),
    );
    request.key = Some("retry-key".into());
    let response = client::rpc(&mock.home, &request, 5).unwrap();
    assert_eq!(response["args"], request.args);
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .iter()
            .map(|r| r.op.as_str())
            .collect::<Vec<_>>(),
        vec!["ping", "inbox"]
    );
    assert_eq!(
        serde_json::to_value(&requests[0][1]).unwrap(),
        serde_json::to_value(&request).unwrap()
    );
}

#[test]
fn ping_and_shutdown_remain_available_without_compatibility_handshake() {
    let mock = Mock::new(vec![hello("0.1.0", json!(1)), json!({})]);
    assert_eq!(
        client::rpc(&mock.home, &Request::new("ping", "", json!({})), 5).unwrap()
            ["protocol_version"],
        1
    );
    assert_eq!(
        client::rpc(&mock.home, &Request::new("shutdown", "", json!({})), 5).unwrap()["op"],
        "shutdown"
    );
    let requests = mock.requests();
    assert_eq!(
        requests
            .iter()
            .map(|c| c[0].op.as_str())
            .collect::<Vec<_>>(),
        vec!["ping", "shutdown"]
    );
    assert!(requests.iter().all(|c| c.len() == 1));
}

#[test]
fn compatibility_is_not_cached_across_connections() {
    let mock = Mock::new(vec![hello("0.2.0", json!(2)), hello("0.1.0", json!(1))]);
    let request = Request::new("inbox", "fixture", json!({"selection":"involved"}));
    client::rpc(&mock.home, &request, 5).unwrap();
    assert_eq!(
        client::rpc(&mock.home, &request, 5).unwrap_err().code,
        "protocol_version"
    );
    let requests = mock.requests();
    assert_eq!(requests[0].len(), 2);
    assert_eq!(requests[1].len(), 1);
}

#[test]
fn start_rejects_old_daemon_without_creating_daemon_artifacts() {
    let mock = Mock::new(vec![hello("0.1.0", json!(1))]);
    let error = client::start(&mock.home, false).unwrap_err();
    assert_eq!(error.code, "protocol_version");
    assert!(error.message.contains("0.1.0"));
    assert!(error.message.contains("stop && fray --home"));
    assert!(error.message.contains(mock.home.to_str().unwrap()));
    assert!(!mock.home.join("daemon.log").exists());
    assert!(!mock.home.join("state.db").exists());
    assert_eq!(mock.requests()[0].len(), 1);
}

#[test]
fn reply_refs_require_capability_before_sending_mutation() {
    let request = Request::new(
        "annotate",
        "fixture",
        json!({"id":1,"body":"Evidence","refs":["mote:42"]}),
    );
    let mock = Mock::new(vec![hello("0.2.1", json!(2))]);
    let error = client::rpc(&mock.home, &request, 5).unwrap_err();
    assert_eq!(error.code, "unsupported_capability");
    assert_eq!(mock.requests()[0].len(), 1);

    let mut metadata = hello("new", json!(2));
    metadata["capabilities"] = json!(["reply_refs"]);
    let mock = Mock::new(vec![metadata]);
    assert_eq!(
        client::rpc(&mock.home, &request, 5).unwrap()["args"],
        request.args
    );
    assert_eq!(mock.requests()[0].len(), 2);
}

#[test]
fn new_message_and_inbox_features_require_capabilities() {
    for (op, args, capability) in [
        (
            "send",
            json!({"to":"peer","body":"x".repeat(2001)}),
            "long_messages",
        ),
        ("inbox", json!({"addressed_to_me":true}), "inbox_filters"),
        ("inbox", json!({"unresolved":true}), "inbox_filters"),
        ("wait", json!({"addressed_to_me":true}), "wait_filters"),
        ("wait", json!({"unresolved":true}), "wait_filters"),
        ("wait", json!({"timeout":null}), "wait_indefinite"),
        ("wait", json!({"kinds":["objection"]}), "attention_filters"),
        ("inbox", json!({"min_priority":1}), "attention_filters"),
    ] {
        let request = Request::new(op, "fixture", args);
        let mock = Mock::new(vec![hello("old", json!(2))]);
        assert_eq!(
            client::rpc(&mock.home, &request, 5).unwrap_err().code,
            "unsupported_capability"
        );
        assert_eq!(mock.requests()[0].len(), 1);
        let mut metadata = hello("new", json!(2));
        metadata["capabilities"] = json!([capability]);
        let mock = Mock::new(vec![metadata]);
        assert_eq!(
            client::rpc(&mock.home, &request, 5).unwrap()["args"],
            request.args
        );
        assert_eq!(mock.requests()[0].len(), 2);
    }
}

#[test]
fn attention_requires_capability_before_subscribing() {
    let mock = Mock::new(vec![hello("old", json!(2))]);
    let error = client::watch_attention(
        &mock.home,
        "fixture",
        json!({"selection":"involved"}),
        false,
    )
    .unwrap_err();
    assert_eq!(error.code, "unsupported_capability");
    assert_eq!(mock.requests()[0].len(), 1);
}

#[test]
fn wait_reports_daemon_disconnect_during_handshake_as_unavailable() {
    let mock = Mock::new(vec![Value::Null]);
    let error = client::rpc(
        &mock.home,
        &Request::new("wait", "fixture", json!({"timeout":null})),
        5,
    )
    .unwrap_err();
    assert_eq!(error.code, "unavailable");
    assert_eq!(mock.requests()[0].len(), 1);
}
