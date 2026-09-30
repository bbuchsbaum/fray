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
fn reservation_events_map_to_their_holder_under_state_keys() {
    let long: Vec<String> = (0..25)
        .map(|i| format!("src/a_rather_long_module_name_{i}/"))
        .collect();
    let events = vec![
        json!({"event_id":"20260930T130000.000000Z-d-reservation-expired-rv-1","type":"reservation.expired","actor":"alice",
            "data":{"holder":"alice","entity":"bd-1","paths":["src/"],"reservation_id":"rv-1","deadline":"D1"}}),
        json!({"event_id":"20260930T125000.000000Z-d-reservation-expiring-rv-2","type":"reservation.expiring","actor":"alice",
            "data":{"holder":"alice","entity":"bd-1","paths":long,"reservation_id":"rv-2","deadline":"D2"}}),
        json!({"event_id":"e3","type":"claim.acquired","actor":"bob","data":{"entity":"bd-2","to":"carol"}}),
    ];
    let items = mote::attention_items(&events);
    let keys: Vec<&str> = items.iter().map(|i| i["key"].as_str().unwrap()).collect();
    assert_eq!(
        keys,
        vec!["rv:rv-1:alice:D1:expired", "rv:rv-2:alice:D2:expiring"]
    );
    // A wide reservation still gives a short title (review of ea93b08).
    let title = items[1]["title"].as_str().unwrap();
    assert!(
        title.len() <= 160 && title.ends_with("and 24 more"),
        "{title}"
    );
}

#[test]
fn claim_events_become_transitions_in_order() {
    let events = vec![
        json!({"op_id":"o1","event_id":"o1","type":"claim.acquired","actor":"alice","data":{"entity":"bd-1","to":"alice"}}),
        json!({"op_id":"o2","event_id":"o2","type":"claim.acquired","actor":"alice","data":{"entity":"bd-1","to":"bob"}}),
        json!({"op_id":"o3","event_id":"o3","type":"claim.released","actor":"bob","data":{"entity":"bd-1"}}),
        json!({"op_id":"o4","event_id":"o4","type":"reservation.opened","actor":"bob","data":{"entity":"bd-1"}}),
    ];
    let t = mote::claim_transitions(&events);
    assert_eq!(t.len(), 3);
    assert_eq!(
        t[1],
        json!({"entity":"bd-1","to":"bob","by":"alice","op_id":"o2"})
    );
    assert_eq!(t[2]["released"], true);
}

fn claim(entity: &str, to: &str, by: &str, op: &str) -> Value {
    json!({"entity":entity,"to":to,"by":by,"op_id":op})
}

fn ingest_claims(b: &mut Board, after: Value, cursor: &str, claims: Vec<Value>) -> Value {
    at(
        b,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-A","after":after,"cursor":cursor,"items":[],"claims":claims}),
    )
    .unwrap()
}

fn mote_titles(b: &mut Board, who: &str) -> Vec<String> {
    at(b, who, "inbox", json!({"selection":"all"})).unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["card"]["author"] == "mote")
        .map(|i| i["card"]["title"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_claim_changing_hands_reaches_the_new_and_the_displaced_holder() {
    let mut b = board();
    at(&mut b, "carol", "join", json!({})).unwrap();
    // Seeded holders produce no cards.
    ingest_claims(
        &mut b,
        Value::Null,
        "c0",
        vec![
            json!({"entity":"bd-1","to":"alice","by":"alice","op_id":"20260930T000000.000000Z","seed":true}),
        ],
    );
    assert!(mote_titles(&mut b, "alice").is_empty());
    // alice hands bd-1 to bob herself: bob hears, alice is not told what she did.
    ingest_claims(
        &mut b,
        json!("c0"),
        "c1",
        vec![claim("bd-1", "bob", "alice", "20260930T010000.000000Z-o1")],
    );
    assert_eq!(
        mote_titles(&mut b, "bob"),
        vec!["Mote: alice handed you bd-1"]
    );
    assert!(mote_titles(&mut b, "alice").is_empty());
    // carol moves bob's claim to herself (Mote has no holder check): bob hears.
    ingest_claims(
        &mut b,
        json!("c1"),
        "c2",
        vec![claim(
            "bd-1",
            "carol",
            "carol",
            "20260930T020000.000000Z-o2",
        )],
    );
    assert!(
        mote_titles(&mut b, "bob").contains(&"Mote: your claim on bd-1 is now carol's".to_owned())
    );
    // A third party hands it on: the new holder and the displaced one both hear.
    ingest_claims(
        &mut b,
        json!("c2"),
        "c3",
        vec![claim("bd-1", "alice", "bob", "20260930T030000.000000Z-o3")],
    );
    assert!(mote_titles(&mut b, "alice").contains(&"Mote: bob handed you bd-1".to_owned()));
    assert!(mote_titles(&mut b, "carol")
        .contains(&"Mote: your claim on bd-1 is now alice's".to_owned()));
    // Replaying the same transitions changes nothing.
    let before = mote_titles(&mut b, "carol").len();
    ingest_claims(
        &mut b,
        json!("c3"),
        "c3",
        vec![claim("bd-1", "alice", "bob", "20260930T030000.000000Z-o3")],
    );
    assert_eq!(mote_titles(&mut b, "carol").len(), before);
}

#[test]
fn an_oversized_or_invalid_item_is_skipped_and_reported_not_fatal() {
    let mut b = board();
    let huge = json!({"key":"k1","recipient":"bob","title":"t".repeat(500),"summary":"s".repeat(5000),"priority":1,"refs":["bd-1"]});
    let bad =
        json!({"key":"k2","recipient":"bob","title":"ok","summary":"s","priority":9,"refs":[]});
    let fine = item("k3", "bob");
    let r = ingest(&mut b, Value::Null, "c", vec![huge, bad, fine]).unwrap();
    // The long one is clipped and delivered; the invalid one is reported;
    // the rest, and the cursor, go through.
    assert_eq!(r["created"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["invalid"][0]["key"], "k2");
    let titles = mote_titles(&mut b, "bob");
    assert!(
        titles.iter().any(|t| t.len() <= 160 && t.ends_with('…')),
        "{titles:?}"
    );
    // The skipped item did not leave a dedupe row: a corrected retry lands.
    let r = ingest(&mut b, json!("c"), "c", vec![item("k2", "bob")]).unwrap();
    assert_eq!(r["created"].as_array().unwrap().len(), 1, "{r}");
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
        let out = cmd.args(args).output().unwrap_or_else(|e| {
            // Rare under heavy concurrent runs (bd-01M3SANZV1KF9PGWSBDFVJV4MD):
            // say which of the working directory and the binary was missing.
            panic!(
                "spawning fray {args:?} failed: {e}; cwd {} exists: {}; binary {} exists: {}",
                self.t.0.display(),
                self.t.0.exists(),
                env!("CARGO_BIN_EXE_fray"),
                std::path::Path::new(env!("CARGO_BIN_EXE_fray")).exists()
            )
        });
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

#[test]
fn review_reproducers_against_the_real_mote() {
    // Review of ea93b08: a wide reservation stalled every later sync, and a
    // displaced holder was never told.
    let Some(p) = Project::new("repro") else {
        return;
    };
    p.sync(&[], "alice").unwrap();
    // A reservation over many long paths expires, next to a handoff.
    let wide = p.bead("alice");
    assert!(p.mote("alice", &["claim", &wide]).status.success());
    let paths: Vec<String> = (0..12)
        .map(|i| format!("src/module_theta_number_{i}/"))
        .collect();
    let mut args = vec!["reserve", "--issue", wide.as_str(), "--ttl", "1"];
    args.extend(paths.iter().map(String::as_str));
    assert!(p.mote("alice", &args).status.success());
    let handed = p.bead("alice");
    assert!(p.mote("alice", &["claim", &handed]).status.success());
    assert!(p
        .mote("alice", &["handoff", &handed, "--to", "bob"])
        .status
        .success());
    // bob moves alice's other claim to himself: Mote has no holder check.
    let taken = p.bead("alice");
    assert!(p.mote("alice", &["claim", &taken]).status.success());
    assert!(p
        .mote("bob", &["handoff", &taken, "--to", "bob"])
        .status
        .success());
    // alice's short claim expires and bob claims it.
    let lapsed = p.bead("alice");
    assert!(p
        .mote("alice", &["claim", &lapsed, "--ttl", "1"])
        .status
        .success());
    std::thread::sleep(Duration::from_millis(1500));
    assert!(p.mote("bob", &["claim", &lapsed]).status.success());

    let s = p.sync(&[], "bob").unwrap();
    assert!(s["invalid"].as_array().unwrap().is_empty(), "{s}");
    let bob = p.titles("bob");
    assert!(
        bob.iter()
            .any(|t| t.contains(&format!("alice handed you {handed}"))),
        "{bob:?}"
    );
    let alice = p.titles("alice");
    assert!(
        alice
            .iter()
            .any(|t| t
                .starts_with("Mote reservation expired on src/module_theta_number_0/ and 11 more")),
        "{alice:?}"
    );
    assert!(
        alice
            .iter()
            .any(|t| t.contains(&format!("your claim on {taken} is now bob's"))),
        "{alice:?}"
    );
    assert!(
        alice
            .iter()
            .any(|t| t.contains(&format!("your claim on {lapsed} is now bob's"))),
        "{alice:?}"
    );
    assert!(alice.iter().all(|t| t.len() <= 160));
}

#[test]
fn replaying_a_chunk_never_invents_a_change_of_hands() {
    // Review of f5e49fa: the same non-final chunk applied twice (an
    // interrupted or concurrent sync) sent bob "your claim is now alice's".
    let mut b = board();
    at(&mut b, "carol", "join", json!({})).unwrap();
    let chunk = vec![
        claim("bd-X", "alice", "alice", "20260930T010000.000000Z-a"),
        claim("bd-X", "bob", "carol", "20260930T020000.000000Z-b"),
    ];
    ingest_claims(&mut b, Value::Null, "c0", chunk.clone());
    let cards = |b: &mut Board| ["alice", "bob", "carol"].map(|w| mote_titles(b, w));
    let first = cards(&mut b);
    assert_eq!(first[1], vec!["Mote: carol handed you bd-X"]);
    assert_eq!(first[0], vec!["Mote: your claim on bd-X is now bob's"]);
    // Replayed, as a sync that did not advance the cursor would.
    let r = ingest_claims(&mut b, json!("c0"), "c0", chunk);
    assert!(r["created"].as_array().unwrap().is_empty(), "{r}");
    assert_eq!(cards(&mut b), first);
}

impl Project {
    /// Moves the board's cursor past everything so far without delivering it:
    /// a stand-in for events the feed skipped (a late op, or a reseed).
    fn skip_events(&self) {
        let (ok, out) = self.fray(&[], "alice", &["--json", "mote", "status"]);
        assert!(ok, "{out}");
        let s: Value = serde_json::from_str(&out).unwrap();
        let cursor = s["mote"]["binding"]["cursor"].as_str().unwrap().to_owned();
        let store_id = s["mote"]["store_id"].as_str().unwrap().to_owned();
        let far = mote::tail_cursor(std::time::SystemTime::now() + Duration::from_secs(3600));
        let req = json!({"op":"mote_ingest","actor":"alice","args":{"store_id":store_id,
            "after":cursor,"cursor":far,"items":[],"claims":[]}});
        let (ok, out) = self.fray(&[], "alice", &["rpc", &req.to_string()]);
        assert!(ok, "{out}");
    }
}

#[test]
fn reconciliation_delivers_claim_changes_the_event_feed_missed_once() {
    let Some(p) = Project::new("recon") else {
        return;
    };
    // alice holds `held` when the board is seeded.
    let held = p.bead("alice");
    assert!(p.mote("alice", &["claim", &held]).status.success());
    p.sync(&[], "alice").unwrap();
    // Changes the feed will miss: a handoff to bob, then bob renewing it (a
    // renewal looks like a handoff in Mote's history), and bob taking
    // alice's claim.
    let handed = p.bead("alice");
    assert!(p.mote("alice", &["claim", &handed]).status.success());
    assert!(p
        .mote("alice", &["handoff", &handed, "--to", "bob"])
        .status
        .success());
    assert!(p.mote("bob", &["claim", &handed]).status.success());
    assert!(p
        .mote("bob", &["handoff", &held, "--to", "bob"])
        .status
        .success());
    p.skip_events();
    let s = p.sync(&[], "alice").unwrap();
    assert_eq!(s["events"], 0, "the feed saw nothing: {s}");
    assert_eq!(s["reconciled_claims"], 2, "{s}");
    let bob = p.titles("bob");
    assert!(
        bob.contains(&format!("Mote: you now hold {handed}")),
        "{bob:?}"
    );
    assert!(
        bob.contains(&format!("Mote: you now hold {held}")),
        "{bob:?}"
    );
    let alice = p.titles("alice");
    assert!(
        alice.contains(&format!("Mote: {held} is now held by bob")),
        "{alice:?}"
    );
    // alice never held `handed` in Fray's record, so she is not told about it.
    assert!(!alice.iter().any(|t| t.contains(&handed)), "{alice:?}");
    // Reconciling again finds nothing to do and delivers nothing twice.
    let s = p.sync(&[], "bob").unwrap();
    assert_eq!(s["reconciled_claims"], 0, "{s}");
    assert_eq!(p.titles("bob").len(), bob.len());
    assert_eq!(p.titles("alice").len(), alice.len());
}

#[test]
fn a_missed_release_is_recorded_quietly_and_never_causes_a_false_receipt() {
    let Some(p) = Project::new("release") else {
        return;
    };
    p.sync(&[], "alice").unwrap();
    let w = p.bead("alice");
    assert!(p.mote("alice", &["claim", &w]).status.success());
    p.sync(&[], "alice").unwrap();
    // alice releases; the feed misses it.
    assert!(p.mote("alice", &["release", &w]).status.success());
    p.skip_events();
    let s = p.sync(&[], "alice").unwrap();
    assert_eq!(s["reconciled_claims"], 1, "{s}");
    assert!(
        s["created"].as_array().unwrap().is_empty(),
        "a release is quiet: {s}"
    );
    // bob claims the free work, seen by the feed: alice is told nothing.
    assert!(p.mote("bob", &["claim", &w]).status.success());
    p.sync(&[], "bob").unwrap();
    assert!(p.titles("alice").is_empty(), "{:?}", p.titles("alice"));
}

#[test]
fn the_event_path_and_reconciliation_share_keys() {
    // A change seen by the feed is not delivered again by reconciliation.
    let Some(p) = Project::new("shared") else {
        return;
    };
    p.sync(&[], "alice").unwrap();
    let w = p.bead("alice");
    assert!(p.mote("alice", &["claim", &w]).status.success());
    assert!(p
        .mote("alice", &["handoff", &w, "--to", "bob"])
        .status
        .success());
    let s = p.sync(&[], "alice").unwrap();
    assert_eq!(s["created"].as_array().unwrap().len(), 1, "{s}");
    assert_eq!(
        s["reconciled_claims"], 0,
        "the table already matches the board: {s}"
    );
    assert_eq!(p.titles("bob").len(), 1);
}

#[test]
fn every_recorded_holder_is_reported_for_reconciliation() {
    let mut b = board();
    ingest_claims(
        &mut b,
        Value::Null,
        "c0",
        vec![
            json!({"entity":"bd-1","to":"alice","by":"alice","op_id":"20260930T000000.000000Z","seed":true}),
        ],
    );
    let r = at(&mut b, "bob", "mote_claims", json!({"store_id":"st-A"})).unwrap();
    assert_eq!(r["holders"]["bd-1"], "alice");
    assert_eq!(r["more"], false);
}

#[test]
fn reconciliation_applies_only_if_fray_still_records_what_the_reader_saw() {
    let mut b = board();
    ingest_claims(
        &mut b,
        Value::Null,
        "c0",
        vec![claim("bd-1", "alice", "alice", "20260930T010000.000000Z-a")],
    );
    let rec = |expect: Value, holder: Value| {
        json!({"store_id":"st-A","after":"c0","cursor":"c0","items":[],"claims":[],
            "reconcile":[{"entity":"bd-1","expect":expect,"holder":holder,"lease_until":"L1","marker":"20260930T020000.000000Z"}]})
    };
    // A reader that saw a different holder than Fray now records loses the race.
    let r = at(
        &mut b,
        "bob",
        "mote_ingest",
        rec(json!("carol"), json!("bob")),
    )
    .unwrap();
    assert_eq!(r["raced"], json!(["bd-1"]));
    assert!(r["created"].as_array().unwrap().is_empty());
    // The right expectation applies, with blame-free cards to both.
    let r = at(
        &mut b,
        "bob",
        "mote_ingest",
        rec(json!("alice"), json!("bob")),
    )
    .unwrap();
    assert_eq!(r["created"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(mote_titles(&mut b, "bob"), vec!["Mote: you now hold bd-1"]);
    assert_eq!(
        mote_titles(&mut b, "alice"),
        vec!["Mote: bd-1 is now held by bob"]
    );
    let all = at(&mut b, "bob", "mote_claims", json!({"store_id":"st-A"})).unwrap();
    assert_eq!(all["holders"]["bd-1"], "bob");
}

#[test]
fn released_claims_are_not_listed_for_reconciliation() {
    // Review of 1f57c49: released rows counted toward the listing limit and
    // could push live claims past it.
    let mut b = board();
    ingest_claims(
        &mut b,
        Value::Null,
        "c0",
        vec![
            claim("bd-1", "alice", "alice", "20260930T010000.000000Z-a"),
            claim("bd-2", "bob", "bob", "20260930T010000.000000Z-b"),
            json!({"entity":"bd-2","to":null,"by":"bob","op_id":"20260930T020000.000000Z-c","released":true}),
        ],
    );
    let r = at(&mut b, "bob", "mote_claims", json!({"store_id":"st-A"})).unwrap();
    assert_eq!(r["holders"], json!({"bd-1":"alice"}));
}
