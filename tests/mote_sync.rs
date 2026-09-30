//! Epic child 5, slice 5b: Mote events become attention exactly once per
//! recipient, with a forward-only cursor (docs/design/mote-adapter.md section 6).
use fray::{
    model::{random_key, Request},
    mote,
    store::Store as Board,
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    time::{Duration, UNIX_EPOCH},
};

const NOW: i64 = 1_800_000_000_000;

fn at(b: &mut Board, actor: &str, op: &str, args: Value) -> Result<Value, String> {
    b.execute_at(&Request::new(op, actor, args), NOW)
        .map_err(|e| e.code)
}

fn board() -> Board {
    let mut b = Board::memory().unwrap();
    for who in ["alice", "bob"] {
        at(&mut b, who, "join", json!({})).unwrap();
    }
    at(
        &mut b,
        "alice",
        "mote_bind",
        json!({"store":"/r/.mote","store_id":"st-A"}),
    )
    .unwrap();
    b
}

fn item(key: &str, to: &str) -> Value {
    json!({"key":key,"recipient":to,"title":format!("t {key}"),"summary":"s","priority":1,"refs":["bd-1"]})
}

fn ingest(b: &mut Board, after: Value, cursor: &str, items: Vec<Value>) -> Result<Value, String> {
    at(
        b,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-A","after":after,"cursor":cursor,"items":items}),
    )
}

#[test]
fn each_event_reaches_each_recipient_exactly_once() {
    let mut b = board();
    let first = ingest(
        &mut b,
        Value::Null,
        "c1",
        vec![item("e1", "bob"), item("e1", "alice")],
    )
    .unwrap();
    assert_eq!(first["created"].as_array().unwrap().len(), 2, "{first}");
    // Replaying the same event (an interrupted sync) creates nothing new.
    let again = ingest(
        &mut b,
        json!("c1"),
        "c1",
        vec![item("e1", "bob"), item("e1", "alice")],
    )
    .unwrap();
    assert_eq!(again["created"].as_array().unwrap().len(), 0);
    assert_eq!(again["duplicate"], 2);
    // The card is authored by the reserved identity, so it reaches even the
    // agent that ran the sync.
    let inbox = at(&mut b, "alice", "inbox", json!({"selection":"all"})).unwrap();
    let card = inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["card"].clone())
        .find(|c| c["title"] == "t e1")
        .expect("alice sees the card her own sync produced");
    assert_eq!(card["author"], "mote");
    assert_eq!(card["assignee"], "alice");
    assert_eq!(card["topic"], "@alice");
    assert!(card["tags"].to_string().contains("mote:bd-1"), "{card}");
}

#[test]
fn the_cursor_moves_forward_only_and_under_compare_and_set() {
    let mut b = board();
    ingest(&mut b, Value::Null, "20260930T120000.000000Z", vec![]).unwrap();
    // A sync that read from a stale cursor writes nothing.
    assert_eq!(
        ingest(
            &mut b,
            Value::Null,
            "20260930T130000.000000Z",
            vec![item("e2", "bob")]
        )
        .unwrap_err(),
        "mote_cursor_moved"
    );
    assert_eq!(
        ingest(
            &mut b,
            json!("20260930T110000.000000Z"),
            "20260930T130000.000000Z",
            vec![]
        )
        .unwrap_err(),
        "mote_cursor_moved"
    );
    let bob = at(&mut b, "bob", "inbox", json!({"selection":"all"})).unwrap();
    assert!(
        bob["items"].as_array().unwrap().is_empty(),
        "nothing written: {bob}"
    );
    // Never backwards.
    assert_eq!(
        ingest(
            &mut b,
            json!("20260930T120000.000000Z"),
            "20260930T115959.000000Z",
            vec![]
        )
        .unwrap_err(),
        "invalid"
    );
    ingest(
        &mut b,
        json!("20260930T120000.000000Z"),
        "20260930T130000.000000Z",
        vec![],
    )
    .unwrap();
    let binding = at(&mut b, "", "mote_binding", json!({})).unwrap();
    assert_eq!(binding["binding"]["cursor"], "20260930T130000.000000Z");
}

#[test]
fn ingest_refuses_an_unbound_or_different_store_and_skips_strangers() {
    let mut b = Board::memory().unwrap();
    at(&mut b, "alice", "join", json!({})).unwrap();
    let unbound = at(
        &mut b,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-A","after":null,"cursor":"c","items":[]}),
    );
    assert_eq!(unbound.unwrap_err(), "mote_store_mismatch");
    let mut b = board();
    let other = at(
        &mut b,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-B","after":null,"cursor":"c","items":[]}),
    );
    assert_eq!(other.unwrap_err(), "mote_store_mismatch");
    // A Mote actor that never joined this board has no inbox here.
    let r = ingest(
        &mut b,
        Value::Null,
        "c",
        vec![item("e3", "carol"), item("e4", "mote")],
    )
    .unwrap();
    assert_eq!(r["unknown_recipients"], json!(["carol", "mote"]));
    assert!(r["created"].as_array().unwrap().is_empty());
    // Nobody can join as the reserved identity.
    assert_eq!(
        at(&mut b, "mote", "join", json!({})).unwrap_err(),
        "reserved_mote"
    );
}

#[test]
fn consecutive_timeouts_are_counted_and_reset_by_a_successful_sync() {
    let mut b = board();
    for n in 1..=3 {
        assert_eq!(
            at(&mut b, "alice", "mote_sync_failed", json!({})).unwrap()["consecutive_timeouts"],
            n
        );
    }
    ingest(&mut b, Value::Null, "c", vec![]).unwrap();
    let binding = at(&mut b, "", "mote_binding", json!({})).unwrap();
    assert_eq!(binding["binding"]["consecutive_timeouts"], 0);
}

#[test]
fn events_map_to_attention_for_the_right_agent() {
    let events = vec![
        json!({"event_id":"e1","type":"reservation.expired","actor":"alice",
            "data":{"holder":"alice","entity":"bd-1","paths":["src/"],"reservation_id":"rv-1","deadline":"D"}}),
        json!({"event_id":"e2","type":"reservation.expiring","actor":"alice",
            "data":{"holder":"alice","entity":"bd-1","paths":["src/","docs/"],"reservation_id":"rv-1"}}),
        // A handoff: bob gives the claim to carol.
        json!({"event_id":"e3","type":"claim.acquired","actor":"bob","data":{"entity":"bd-2","to":"carol"}}),
        // Claiming for oneself is not news.
        json!({"event_id":"e4","type":"claim.acquired","actor":"bob","data":{"entity":"bd-3","to":"bob"}}),
        json!({"event_id":"e5","type":"reservation.opened","actor":"bob","data":{}}),
    ];
    let items = mote::attention_items(&events);
    let got: Vec<(String, String)> = items
        .iter()
        .map(|i| {
            (
                i["key"].as_str().unwrap().to_owned(),
                i["recipient"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("e1".into(), "alice".into()),
            ("e2".into(), "alice".into()),
            ("e3".into(), "carol".into())
        ]
    );
    assert!(items[1]["title"].as_str().unwrap().contains("src/, docs/"));
    assert!(items[2]["title"]
        .as_str()
        .unwrap()
        .contains("bob handed you bd-2"));
}

#[test]
fn the_tail_cursor_is_op_id_shaped_and_orders_with_real_ids() {
    let t = UNIX_EPOCH + Duration::from_micros(1_790_766_704_123_456);
    let c = mote::tail_cursor(t);
    assert_eq!(c, "20260930T111144.123456Z");
    assert!(c.as_str() < "20260930T111144.123457Z-p1-c0000-r0000-h000000");
    assert!(c.as_str() > "20260930T111144.123455Z-p1-c0000-r0000-h000000");
    assert_eq!(mote::tail_cursor(UNIX_EPOCH), "19700101T000000.000000Z");
    // A leap day.
    let leap = UNIX_EPOCH + Duration::from_secs(951_782_400);
    assert_eq!(mote::tail_cursor(leap), "20000229T000000.000000Z");
}

// ---- The CLI against the real mote, when installed ----

struct Temp(PathBuf);
impl Temp {
    fn new(tag: &str) -> Self {
        let p =
            PathBuf::from("/tmp").join(format!("fray-sync-{tag}-{}", &random_key().unwrap()[..10]));
        fs::create_dir_all(&p).unwrap();
        Self(fs::canonicalize(&p).unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Project {
    t: Temp,
}
impl Project {
    fn new(tag: &str) -> Option<Self> {
        if Command::new("mote").arg("--version").output().is_err() {
            eprintln!("mote not installed; skipping");
            return None;
        }
        let t = Temp::new(tag);
        fs::create_dir_all(t.0.join(".fray")).unwrap();
        let p = Self { t };
        assert!(p.mote("alice", &["init"]).status.success());
        p.fray(&[], "", &["start"]);
        for who in ["alice", "bob"] {
            p.fray(&[], who, &["join"]);
        }
        Some(p)
    }
    /// The real mote on this project's store only, never on one the caller's
    /// environment names.
    fn mote(&self, actor: &str, args: &[&str]) -> std::process::Output {
        Command::new("mote")
            .current_dir(&self.t.0)
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_ACTOR")
            .arg("--store")
            .arg(self.t.0.join(".mote"))
            .args(["--actor", actor])
            .args(args)
            .output()
            .unwrap()
    }
    fn fray(&self, env: &[(&str, &str)], actor: &str, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fray"));
        cmd.current_dir(&self.t.0)
            .env_remove("FRAY_AGENT")
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_ACTOR")
            .env("FRAY_SESSION", format!("test:{actor}"))
            .args(["--home", self.t.0.join(".fray").to_str().unwrap()]);
        if !actor.is_empty() {
            cmd.args(["--as", actor]);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.args(args).output().unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
    fn sync(&self, env: &[(&str, &str)], actor: &str) -> Result<Value, String> {
        let (ok, out) = self.fray(env, actor, &["--json", "mote", "sync"]);
        if ok {
            Ok(
                serde_json::from_str::<Value>(&out).map_err(|e| format!("{e}: {out}"))?
                    ["mote_sync"]
                    .clone(),
            )
        } else {
            Err(out)
        }
    }
    fn titles(&self, actor: &str) -> Vec<String> {
        let (_, out) = self.fray(&[], actor, &["--json", "inbox", "--selection", "all"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["card"]["author"] == "mote")
            .map(|i| i["card"]["title"].as_str().unwrap().to_owned())
            .collect()
    }
    fn bead(&self, actor: &str) -> String {
        let out = self.mote(actor, &["new", "work"]);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.split_whitespace()
            .find(|w| w.starts_with("bd-"))
            .unwrap_or_else(|| panic!("no bead id in {text}"))
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '-')
            .to_owned()
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        self.fray(&[], "", &["stop"]);
    }
}

#[test]
fn a_handoff_and_an_expiry_reach_their_agents_once_and_strangers_are_reported() {
    let Some(p) = Project::new("real") else {
        return;
    };
    // The first sync starts at the tail: history is never replayed.
    let early = p.bead("alice");
    assert!(p.mote("alice", &["claim", &early]).status.success());
    let s = p.sync(&[], "alice").unwrap();
    assert!(s["seeded"].is_string(), "{s}");
    assert!(p.titles("alice").is_empty() && p.titles("bob").is_empty());

    // alice hands a claim to bob; another to carol, who is not on this board.
    let work = p.bead("alice");
    assert!(p.mote("alice", &["claim", &work]).status.success());
    assert!(p
        .mote("alice", &["handoff", &work, "--to", "bob"])
        .status
        .success());
    let other = p.bead("alice");
    assert!(p.mote("alice", &["claim", &other]).status.success());
    assert!(p
        .mote("alice", &["handoff", &other, "--to", "carol"])
        .status
        .success());
    // A reservation that expires almost at once.
    let res = p.bead("alice");
    assert!(p.mote("alice", &["claim", &res]).status.success());
    assert!(p
        .mote(
            "alice",
            &["reserve", "--issue", &res, "--ttl", "1", "docs/"]
        )
        .status
        .success());
    std::thread::sleep(Duration::from_millis(1500));

    // Synced by bob: alice still hears about her own expiry.
    let s = p.sync(&[], "bob").unwrap();
    assert_eq!(s["created"].as_array().unwrap().len(), 2, "{s}");
    assert_eq!(s["unknown_recipients"], json!(["carol"]));
    let bob = p.titles("bob");
    assert!(
        bob.iter()
            .any(|t| t.contains(&format!("alice handed you {work}"))),
        "{bob:?}"
    );
    let alice = p.titles("alice");
    assert!(
        alice
            .iter()
            .any(|t| t.contains("reservation expired on docs/")),
        "{alice:?}"
    );

    // Syncing again, by anyone, delivers nothing twice.
    let s = p.sync(&[], "alice").unwrap();
    assert!(s["created"].as_array().unwrap().is_empty(), "{s}");
    assert_eq!(p.titles("bob").len(), bob.len());
    assert_eq!(p.titles("alice").len(), alice.len());
}

#[test]
fn three_timeouts_in_a_row_reseed_at_the_tail() {
    let Some(p) = Project::new("slow") else {
        return;
    };
    p.sync(&[], "alice").unwrap();
    // A mote whose events never finish; everything else is the real one.
    let stub = p.t.0.join("slow-mote");
    fs::write(
        &stub,
        "#!/bin/sh\ncase \"$*\" in *events*) sleep 30;; *) exec mote \"$@\";; esac\n",
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let env = [
        ("FRAY_MOTE_BIN", stub.to_str().unwrap()),
        ("FRAY_MOTE_READ_TIMEOUT_MS", "300"),
    ];
    for n in 1..=2 {
        let err = p.sync(&env, "alice").unwrap_err();
        assert!(err.contains(&format!("({n} of 3")), "{err}");
    }
    let s = p.sync(&env, "alice").unwrap();
    assert!(s["reseeded"].is_string(), "{s}");
    assert!(s["reseeded"].as_str() >= s["skipped_from"].as_str());
    // A healthy sync afterwards works from the new cursor.
    let s = p.sync(&[], "alice").unwrap();
    assert!(s["created"].is_array(), "{s}");
}

#[test]
fn sync_needs_an_identity_and_a_paired_store() {
    let t = Temp::new("none");
    fs::create_dir_all(t.0.join(".fray")).unwrap();
    let p = Project { t };
    p.fray(&[], "", &["start"]);
    p.fray(&[], "alice", &["join"]);
    let (ok, out) = p.fray(&[], "", &["mote", "sync"]);
    assert!(!ok && out.contains("identity"), "{out}");
    let s = p.sync(&[], "alice").unwrap();
    assert_eq!(s["adopted"], false, "{s}");
}

#[test]
fn brief_warns_when_mote_actor_names_someone_else() {
    let t = Temp::new("warn");
    fs::create_dir_all(t.0.join(".fray")).unwrap();
    let p = Project { t };
    p.fray(&[], "", &["start"]);
    p.fray(&[], "alice", &["join"]);
    let (_, out) = p.fray(&[("MOTE_ACTOR", "mallory")], "alice", &["brief"]);
    assert!(out.contains("MOTE_ACTOR is mallory"), "{out}");
    let (_, out) = p.fray(&[("MOTE_ACTOR", "alice")], "alice", &["brief"]);
    assert!(!out.contains("MOTE_ACTOR is"), "{out}");
}
