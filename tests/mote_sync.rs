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
        let p = PathBuf::from("/tmp").join(format!(
            "fray-sync-{tag}-{}-{}",
            std::process::id(),
            random_key().unwrap()
        ));
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

fn candidate(
    landable: bool,
    codes: &[&str],
    reviews: Value,
    phase: &str,
    policy_op: &str,
) -> Value {
    json!({"candidate_id":"cand-1","entity":"bd-9","proposer":"bob",
        "policy":{"authorizer":"alice","reviewers":["carol"],"op_id":policy_op},
        "phase":{"value":phase,"op_id":"P1"},
        "reviews":reviews,
        "identity":{"commit_oid":"0123456789abcdef"},
        "landability":{"landable":landable,"reason_codes":codes,
            "reasons":codes.iter().map(|c| json!({"blocking":true,"code":c,"detail":"d"})).collect::<Vec<_>>()}})
}

fn to_and_keys(items: &[Value]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|i| {
            (
                i["recipient"].as_str().unwrap().to_owned(),
                i["key"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn a_pending_candidate_asks_unreviewed_reviewers_and_reports_landability() {
    let c = candidate(false, &["review_missing"], json!({}), "pending", "POL1");
    let items = mote::candidate_items(&c);
    let got = to_and_keys(&items);
    assert_eq!(
        got[0],
        ("carol".into(), "cand-review:cand-1:carol:POL1".into())
    );
    assert_eq!(got[1].0, "bob");
    assert_eq!(got[2].0, "alice");
    assert_eq!(got[1].1, got[2].1, "one status key for everyone told");
    assert!(items[1]["title"]
        .as_str()
        .unwrap()
        .contains("blocked (review_missing)"));
    // Once carol reviews, she is not asked again; landability changing
    // (without a new phase op) gives a new status key.
    let reviewed = candidate(
        true,
        &[],
        json!({"carol":{"verdict":"approve","op_id":"R1"}}),
        "pending",
        "POL1",
    );
    let after = mote::candidate_items(&reviewed);
    assert!(after.iter().all(|i| i["recipient"] != "carol"));
    assert_ne!(after[0]["key"], items[1]["key"]);
    assert!(after[0]["title"].as_str().unwrap().ends_with("is landable"));
    // Evidence that changes nothing keeps the same status key: no new card.
    let same = candidate(
        true,
        &[],
        json!({"carol":{"verdict":"approve","op_id":"R1"}}),
        "pending",
        "POL1",
    );
    assert_eq!(mote::candidate_items(&same)[0]["key"], after[0]["key"]);
    // An amended policy asks again.
    let amended = candidate(false, &["review_missing"], json!({}), "pending", "POL2");
    assert_eq!(
        mote::candidate_items(&amended)[0]["key"],
        "cand-review:cand-1:carol:POL2"
    );
}

#[test]
fn a_candidate_leaving_pending_tells_everyone_involved_once() {
    let landed = candidate(
        true,
        &[],
        json!({"carol":{"verdict":"approve","op_id":"R1"}}),
        "landed",
        "POL1",
    );
    let got = to_and_keys(&mote::candidate_items(&landed));
    let who: Vec<&str> = got.iter().map(|(w, _)| w.as_str()).collect();
    assert_eq!(who, vec!["bob", "alice", "carol"]);
    assert!(got.iter().all(|(_, k)| k == "cand:cand-1:P1:landed"));
    let events = vec![
        json!({"type":"candidate.landed","data":{"candidate_id":"cand-1"}}),
        json!({"type":"candidate.reviewed","data":{"candidate_id":"cand-2"}}),
        json!({"type":"candidate.landed","data":{"candidate_id":"cand-1"}}),
    ];
    assert_eq!(mote::terminal_candidates(&events), vec!["cand-1"]);
}

#[test]
fn candidates_reach_their_reviewer_and_proposer_through_the_real_mote() {
    let Some(p) = Project::new("cand") else {
        return;
    };
    p.fray(&[], "carol", &["join"]);
    // A candidate needs a commit in the repository that backs the store.
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&p.t.0)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    git(&["commit", "-q", "--allow-empty", "-m", "work"]);
    let head = git(&["rev-parse", "HEAD"]);
    let work = p.bead("bob");
    assert!(p.mote("bob", &["claim", &work]).status.success());
    p.sync(&[], "alice").unwrap();
    let out = p.mote(
        "bob",
        &[
            "candidate",
            "propose",
            "--issue",
            &work,
            "--commit",
            &head,
            "--base",
            &base,
            "--reviewer",
            "carol",
            "--authorizer",
            "alice",
            "--idempotency-key",
            "k1",
            "--path",
            "src/",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = p.sync(&[], "alice").unwrap();
    assert!(s["candidate_note"].is_null(), "{s}");
    let carol = p.titles("carol");
    assert!(
        carol
            .iter()
            .any(|t| t.starts_with("Mote: review requested on cand-")),
        "{carol:?}"
    );
    let bob = p.titles("bob");
    assert!(bob.iter().any(|t| t.contains("is blocked")), "{bob:?}");
    // A second sync delivers nothing twice.
    let before = (p.titles("carol").len(), p.titles("bob").len());
    p.sync(&[], "bob").unwrap();
    assert_eq!((p.titles("carol").len(), p.titles("bob").len()), before);
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

#[test]
fn a_candidate_returning_to_an_earlier_state_is_reported_again() {
    // Review of 05dfbc2: landable -> blocked -> landable within one phase
    // re-used a delivered key, so bob's newest card said "blocked".
    let mut b = board();
    at(&mut b, "carol", "join", json!({})).unwrap();
    ingest(&mut b, Value::Null, "c0", vec![]).unwrap();
    let reviewed = json!({"carol":{"verdict":"approve","op_id":"R1"}});
    let states = [
        candidate(true, &[], reviewed.clone(), "pending", "POL1"),
        candidate(
            false,
            &["authorization_revoked"],
            reviewed.clone(),
            "pending",
            "POL1",
        ),
        candidate(true, &[], reviewed.clone(), "pending", "POL1"),
    ];
    for c in &states {
        ingest(&mut b, json!("c0"), "c0", mote::candidate_items(c)).unwrap();
    }
    let bob = mote_titles(&mut b, "bob");
    assert_eq!(
        bob,
        vec![
            "Mote: cand-1 is landable",
            "Mote: cand-1 is blocked (authorization_revoked)",
            "Mote: cand-1 is landable"
        ],
        "{bob:?}"
    );
    // Re-reading the same state (evidence that changes nothing) sends nothing.
    let r = ingest(&mut b, json!("c0"), "c0", mote::candidate_items(&states[2])).unwrap();
    assert!(r["created"].as_array().unwrap().is_empty(), "{r}");
}

#[test]
fn blocked_reasons_name_their_subject() {
    let mut c = candidate(false, &["review_missing"], json!({}), "pending", "POL1");
    c["landability"]["reasons"][0]["subject"] = json!("dave");
    let items = mote::candidate_items(&c);
    let status = items.iter().find(|i| i["recipient"] == "bob").unwrap();
    assert!(
        status["summary"]
            .as_str()
            .unwrap()
            .contains("review_missing (dave)"),
        "{status}"
    );
    // A different missing reviewer is a different state.
    let mut other = c.clone();
    other["landability"]["reasons"][0]["subject"] = json!("erin");
    let other_status = mote::candidate_items(&other)
        .into_iter()
        .find(|i| i["recipient"] == "bob")
        .unwrap();
    assert_ne!(status["key"], other_status["key"]);
}

#[test]
fn a_candidate_that_is_revoked_and_reauthorized_through_the_real_mote_is_reported_each_time() {
    let Some(p) = Project::new("cand-aba") else {
        return;
    };
    p.fray(&[], "carol", &["join"]);
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&p.t.0)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    git(&["commit", "-q", "--allow-empty", "-m", "work"]);
    let head = git(&["rev-parse", "HEAD"]);
    let work = p.bead("bob");
    assert!(p.mote("bob", &["claim", &work]).status.success());
    p.sync(&[], "alice").unwrap();
    let out = p.mote(
        "bob",
        &[
            "--json",
            "candidate",
            "propose",
            "--issue",
            &work,
            "--commit",
            &head,
            "--base",
            &base,
            "--reviewer",
            "carol",
            "--authorizer",
            "alice",
            "--idempotency-key",
            "k1",
            "--path",
            "src/",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cand = serde_json::from_slice::<Value>(&out.stdout).unwrap()["candidate_id"]
        .as_str()
        .unwrap()
        .to_owned();
    p.sync(&[], "alice").unwrap();
    let last = |p: &Project| p.titles("bob").last().cloned().unwrap_or_default();
    // Each step changes the candidate's state; bob's newest card must match it.
    let step = |p: &Project, actor: &str, args: &[&str]| {
        let out = p.mote(actor, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        p.sync(&[], "alice").unwrap();
    };
    step(
        &p,
        "carol",
        &[
            "candidate",
            "review",
            &cand,
            "block",
            "--idempotency-key",
            "r1",
        ],
    );
    assert!(
        last(&p).contains("review_blocking"),
        "{:?}",
        p.titles("bob")
    );
    // A re-review names the reviewer's previous review (compare-and-set).
    let shown = p.mote("carol", &["--json", "candidate", "show", &cand]);
    let prior = serde_json::from_slice::<Value>(&shown.stdout).unwrap()["reviews"]["carol"]
        ["op_id"]
        .as_str()
        .unwrap()
        .to_owned();
    step(
        &p,
        "carol",
        &[
            "candidate",
            "review",
            &cand,
            "approve",
            "--expect",
            &prior,
            "--idempotency-key",
            "r2",
        ],
    );
    assert!(
        !last(&p).contains("review_blocking"),
        "{:?}",
        p.titles("bob")
    );
    let blocked_after_approve = last(&p);
    let auth_op = |p: &Project| {
        let shown = p.mote("alice", &["--json", "candidate", "show", &cand]);
        serde_json::from_slice::<Value>(&shown.stdout).unwrap()["authorization"]["op_id"]
            .as_str()
            .map(str::to_owned)
    };
    step(
        &p,
        "alice",
        &[
            "candidate",
            "authorize",
            &cand,
            "--grantee",
            "bob",
            "--idempotency-key",
            "a1",
        ],
    );
    let after_authorize = last(&p);
    assert_ne!(
        after_authorize,
        blocked_after_approve,
        "{:?}",
        p.titles("bob")
    );
    let op = auth_op(&p).expect("authorized");
    step(
        &p,
        "alice",
        &[
            "candidate",
            "revoke",
            &cand,
            "--expect",
            &op,
            "--idempotency-key",
            "a2",
        ],
    );
    assert!(
        last(&p).contains("authorization_revoked"),
        "{:?}",
        p.titles("bob")
    );
    let op = auth_op(&p).expect("revocation recorded");
    step(
        &p,
        "alice",
        &[
            "candidate",
            "authorize",
            &cand,
            "--grantee",
            "bob",
            "--expect",
            &op,
            "--idempotency-key",
            "a3",
        ],
    );
    assert_eq!(
        last(&p),
        after_authorize,
        "back to the earlier state is reported again: {:?}",
        p.titles("bob")
    );
}

#[test]
fn a_final_candidate_state_is_never_replaced_by_an_older_one() {
    // Review of 12cedd8: a slower sync that listed the candidate as pending
    // could deliver that state after another delivered "landed".
    let mut b = board();
    at(&mut b, "carol", "join", json!({})).unwrap();
    ingest(&mut b, Value::Null, "c0", vec![]).unwrap();
    let reviewed = json!({"carol":{"verdict":"approve","op_id":"R1"}});
    let landed = candidate(true, &[], reviewed.clone(), "landed", "POL1");
    ingest(&mut b, json!("c0"), "c0", mote::candidate_items(&landed)).unwrap();
    let stale = candidate(true, &[], reviewed, "pending", "POL1");
    let r = ingest(&mut b, json!("c0"), "c0", mote::candidate_items(&stale)).unwrap();
    assert!(
        r["created"].as_array().unwrap().is_empty(),
        "nothing after a final state: {r}"
    );
    assert_eq!(mote_titles(&mut b, "bob"), vec!["Mote: cand-1 is landed"]);
}

#[test]
fn a_watching_agent_hears_mote_changes_without_anyone_running_sync() {
    // Background sync (section 6): a runner syncs Mote itself, paced for the
    // whole board, so an armed listener hears a handoff with no manual sync.
    let Some(p) = Project::new("bg") else {
        return;
    };
    let mut watch = Command::new(env!("CARGO_BIN_EXE_fray"))
        .current_dir(&p.t.0)
        .env_remove("FRAY_AGENT")
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_ACTOR")
        .env("FRAY_SESSION", "test:bob")
        .env("FRAY_MOTE_SYNC_INTERVAL_MS", "300")
        .args([
            "--home",
            p.t.0.join(".fray").to_str().unwrap(),
            "--as",
            "bob",
        ])
        .args(["watch", "--attention", "--notification"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = watch.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    // Wait until the background sync has run once (seeding the cursor), so
    // the handoff below is new to it.
    let seeded = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let (ok, out) = p.fray(
            &[],
            "bob",
            &["rpc", r#"{"op":"mote_binding","actor":"bob","args":{}}"#],
        );
        let synced = serde_json::from_str::<serde_json::Value>(&out)
            .ok()
            .is_some_and(|v| v["binding"]["last_sync_ms"].is_i64());
        if ok && synced {
            break;
        }
        assert!(
            std::time::Instant::now() < seeded,
            "the background sync never ran: {out}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let w = p.bead("alice");
    assert!(p.mote("alice", &["claim", &w]).status.success());
    assert!(p
        .mote("alice", &["handoff", &w, "--to", "bob"])
        .status
        .success());
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut heard = None;
    while std::time::Instant::now() < deadline {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) {
            // Either path delivers it: the handoff event, or, when one sync
            // reads events just before the handoff and the board just after,
            // reconciliation (section 6), which then supersedes the event.
            if line.contains(&format!("alice handed you {w}"))
                || line.contains(&format!("you now hold {w}"))
            {
                heard = Some(line);
                break;
            }
        }
    }
    let _ = watch.kill();
    let _ = watch.wait();
    assert!(heard.is_some(), "bob's watch never showed the handoff");
}

/// No silent stalls R2: an open Mote request reaches its Fray addressee as an
/// ask, once; when it is answered in Mote the card settles; notes are not
/// carded; a request to an actor not on the board is reported unknown.
#[test]
fn a_mote_request_is_carded_once_and_settles_when_answered_in_mote() {
    let Some(p) = Project::new("mreq") else {
        return;
    };
    p.sync(&[], "alice").unwrap();
    let send = |from: &str, to: &str, kind: &str, body: &str| -> String {
        let out = p.mote(
            from,
            &["msg", "send", "--to", to, "--kind", kind, body, "--json"],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["msg_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let req = send("alice", "bob", "request", "Review abc123 please");
    send("alice", "bob", "note", "just so you know");
    send("alice", "zed", "request", "to someone not on the board");
    let r = p.sync(&[], "alice").unwrap();
    assert!(
        r["unknown_recipients"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u == "zed"),
        "{r}"
    );
    let cards = |p: &Project| -> Vec<Value> {
        let (ok, out) = p.fray(
            &[],
            "bob",
            &["--json", "query", "--all", "--ref", &format!("mote:{req}")],
        );
        assert!(ok, "{out}");
        serde_json::from_str::<Value>(&out).unwrap()["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    assert_eq!(r["created"].as_array().map_or(0, Vec::len), 1, "{r}");
    let found = cards(&p);
    assert_eq!(found.len(), 1, "{found:?}");
    let card = &found[0];
    assert_eq!(card["kind"], "question", "{card}");
    assert_eq!(card["assignee"], "bob", "{card}");
    assert_eq!(card["status"], "open", "{card}");
    assert!(card["summary"]
        .as_str()
        .unwrap()
        .contains("not a Mote answer"));
    // Notes are not carded.
    assert!(
        !p.titles("bob")
            .iter()
            .any(|t| t.contains("just so you know")),
        "{:?}",
        p.titles("bob")
    );
    // A second sync cards nothing new.
    p.sync(&[], "bob").unwrap();
    assert_eq!(cards(&p).len(), 1);
    // bob answers in Mote: the card settles.
    assert!(p
        .mote("bob", &["msg", "reply", &req, "looks good"])
        .status
        .success());
    let r = p.sync(&[], "alice").unwrap();
    assert!(
        r["request_note"]
            .as_str()
            .unwrap_or("")
            .contains("settled 1"),
        "{r}"
    );
    let found = cards(&p);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["status"], "resolved", "{:?}", found[0]);
    // A Fray send to zed, known only to Mote, says how to ask there.
    let (ok, out) = p.fray(&[], "alice", &["send", "zed", "hello", "--ask"]);
    assert!(!ok);
    assert!(
        out.contains("zed is a Mote actor") && out.contains("mote msg send --to zed"),
        "{out}"
    );
}

/// Review of 405e671, #79: a request Fray cannot card as-is (a NUL in its
/// body) never stops the others.
#[test]
fn one_malformed_mote_request_does_not_stop_the_rest() {
    let Some(p) = Project::new("mreqbad") else {
        return;
    };
    p.sync(&[], "alice").unwrap();
    // A NUL cannot go through argv; mote reads it literally from stdin.
    let mut bad = Command::new("mote")
        .current_dir(&p.t.0)
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_ACTOR")
        .arg("--store")
        .arg(p.t.0.join(".mote"))
        .args([
            "--actor", "alice", "msg", "send", "--to", "bob", "--kind", "request", "--stdin",
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        bad.stdin.take().unwrap().write_all(b"bad\0body").unwrap();
    }
    assert!(bad.wait().unwrap().success());
    let out = p.mote(
        "alice",
        &[
            "msg",
            "send",
            "--to",
            "bob",
            "--kind",
            "request",
            "a normal request",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = p.sync(&[], "alice").unwrap();
    assert_eq!(r["created"].as_array().map_or(0, Vec::len), 2, "{r}");
}

/// Review of 405e671, #80: Mote actors who never joined cannot crowd out a
/// board agent's requests, however many there are.
#[test]
fn board_agents_requests_are_read_before_mote_only_actors() {
    let Some(p) = Project::new("mreqmany") else {
        return;
    };
    p.sync(&[], "alice").unwrap();
    for n in 1..=55 {
        let to = format!("a{n:02}");
        assert!(p
            .mote(
                "alice",
                &["msg", "send", "--to", &to, "--kind", "request", "x"]
            )
            .status
            .success());
    }
    assert!(p
        .mote(
            "alice",
            &["msg", "send", "--to", "bob", "--kind", "request", "for bob"]
        )
        .status
        .success());
    let r = p.sync(&[], "alice").unwrap();
    assert_eq!(r["created"].as_array().map_or(0, Vec::len), 1, "{r}");
    assert!(
        r["request_note"]
            .as_str()
            .unwrap_or("")
            .contains("56 addressees; 50 read"),
        "{r}"
    );
}

/// #79 at the daemon: an item that cannot be carded is skipped and
/// reported; the rest of the call still lands.
#[test]
fn the_daemon_skips_an_uncardable_request_and_cards_the_rest() {
    let mut b = board();
    let r = b.execute_at(&Request::new(
        "mote_requests_sync",
        "alice",
        json!({"store_id":"st-A","requests":[
            {"msg_id":"x".repeat(300),"recipient":"bob","from":"alice","state":"open","body":"b"},
            {"msg_id":"msg-ok","recipient":"bob","from":"alice","state":"open","body":"fine"}
        ]})), NOW)
    .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(r["created"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["invalid"].as_array().unwrap().len(), 1, "{r}");
}

/// No silent stalls R3, the incident replayed end to end: a Mote-only
/// request to an agent nothing can wake. With no runner, `fray stuck` lists
/// it; with an armed steward's runner alive and no other activity, the
/// steward's own listener is woken with the escalation.
#[test]
fn the_incident_reaches_a_present_steward_and_the_stuck_list() {
    let Some(p) = Project::new("incident") else {
        return;
    };
    let grace = [("FRAY_STUCK_GRACE_MS", "0")];
    // The daemon reads the grace from its environment.
    p.fray(&[], "", &["stop"]);
    p.fray(&grace, "", &["start"]);
    p.fray(&[], "helper", &["join"]);
    p.fray(&[], "steward", &["join", "--role", "steward"]);
    p.sync(&[], "alice").unwrap();
    let out = p.mote(
        "alice",
        &[
            "msg",
            "send",
            "--to",
            "helper",
            "--kind",
            "request",
            "SHA-bound review of 1a2b3c?",
        ],
    );
    assert!(out.status.success());
    // No runner alive: the stuck list still finds it, straight from Mote.
    let (ok, out) = p.fray(&grace, "", &["--json", "stuck"]);
    assert!(ok, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ticking"], false, "{v}");
    let unseen = v["mote_not_on_board"].as_array().unwrap();
    assert!(
        unseen
            .iter()
            .any(|m| m["to"] == "helper" && m["from"] == "alice"),
        "{v}"
    );
    // The steward arms an involved listener; its runner syncs and ticks.
    let mut watch = Command::new(env!("CARGO_BIN_EXE_fray"))
        .current_dir(&p.t.0)
        .env_remove("FRAY_AGENT")
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_ACTOR")
        .env("FRAY_SESSION", "test:steward")
        .env("FRAY_MOTE_SYNC_INTERVAL_MS", "300")
        .env("FRAY_STUCK_GRACE_MS", "0")
        .args([
            "--home",
            p.t.0.join(".fray").to_str().unwrap(),
            "--as",
            "steward",
        ])
        .args([
            "watch",
            "--attention",
            "--notification",
            "--selection",
            "involved",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = watch.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut heard = None;
    while std::time::Instant::now() < deadline {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) {
            if line.contains("Stuck (unreachable): Mote request from alice") {
                heard = Some(line);
                break;
            }
        }
    }
    let _ = watch.kill();
    let _ = watch.wait();
    if heard.is_some() {
        // #85: now carded, it is listed from the board, not as unsynced.
        let (_, out) = p.fray(&grace, "", &["--json", "stuck"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["mote_not_on_board"].as_array().unwrap().is_empty(), "{v}");
    }
    if heard.is_none() {
        let rest: Vec<String> = rx.try_iter().collect();
        let (_, stuck) = p.fray(&grace, "", &["--json", "stuck"]);
        let (_, inbox) = p.fray(&[], "steward", &["--json", "inbox", "--selection", "all"]);
        panic!("the steward's listener never heard the escalation; lines {rest:?}; stuck {stuck}; inbox {inbox}");
    }
}

/// Review of 39ca448, #84: a request to an actor who never joined stops
/// being stuck as soon as it is answered in Mote.
#[test]
fn an_answered_request_to_an_unjoined_actor_is_no_longer_stuck() {
    let Some(p) = Project::new("unknownans") else {
        return;
    };
    let grace = [("FRAY_STUCK_GRACE_MS", "0")];
    p.fray(&[], "", &["stop"]);
    p.fray(&grace, "", &["start"]);
    p.sync(&[], "alice").unwrap();
    let out = p.mote(
        "alice",
        &[
            "msg", "send", "--to", "zed", "--kind", "request", "x", "--json",
        ],
    );
    let msg = serde_json::from_slice::<Value>(&out.stdout).unwrap()["msg_id"]
        .as_str()
        .unwrap()
        .to_owned();
    p.sync(&grace, "alice").unwrap();
    let stuck = |p: &Project| -> Value {
        let (_, out) = p.fray(&grace, "", &["--json", "stuck"]);
        serde_json::from_str(&out).unwrap()
    };
    assert_eq!(
        stuck(&p)["stuck"].as_array().unwrap().len(),
        1,
        "{}",
        stuck(&p)
    );
    assert!(p
        .mote("zed", &["msg", "reply", &msg, "done"])
        .status
        .success());
    p.sync(&grace, "alice").unwrap();
    let v = stuck(&p);
    assert!(v["stuck"].as_array().unwrap().is_empty(), "{v}");
}

/// `fray team`: the roster with roles, hosts and reachability, ready beads
/// nobody has claimed (claimed ones left out), and the gaps.
#[test]
fn fray_team_shows_roles_unclaimed_work_and_gaps() {
    let Some(p) = Project::new("team") else {
        return;
    };
    p.fray(&[], "lead", &["join", "--role", "steward"]);
    let free = p.bead("alice");
    let taken = p.bead("alice");
    assert!(p.mote("bob", &["claim", &taken]).status.success());
    let (ok, out) = p.fray(&[], "alice", &["--json", "team"]);
    assert!(ok, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let t = &v["team"];
    let lead = t["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "lead")
        .unwrap();
    assert_eq!(lead["role"], "steward", "{t}");
    let ready: Vec<&str> = t["ready_unclaimed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["id"].as_str())
        .collect();
    assert!(ready.contains(&free.as_str()), "{t}");
    assert!(
        !ready.contains(&taken.as_str()),
        "claimed beads are left out: {t}"
    );
    // lead is a steward who just joined: present, so alive (no gap), and
    // nobody should take the steward role from it (#98).
    assert_eq!(t["steward_alive"], true, "{t}");
    let gaps = t["gaps"].to_string();
    assert!(
        gaps.contains("no Team card") && !gaps.contains("steward"),
        "{gaps}"
    );
    // The text form names the same things.
    let (_, text) = p.fray(&[], "alice", &["team"]);
    assert!(
        text.contains("Ready, unclaimed: ") && text.contains("Gaps:"),
        "{text}"
    );
}

/// Review of d9a64e9, #95: when Mote cannot be read, `fray team` says so;
/// it never presents unknown work as no work.
#[test]
fn fray_team_reports_an_unreadable_mote_as_unknown() {
    let Some(p) = Project::new("teamfail") else {
        return;
    };
    p.bead("alice");
    let (ok, out) = p.fray(
        &[("FRAY_MOTE_BIN", "/usr/bin/false")],
        "alice",
        &["--json", "team"],
    );
    assert!(ok, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let t = &v["team"];
    assert!(t["ready_unclaimed"].is_null(), "{t}");
    assert!(t["reviews_waiting"].is_null(), "{t}");
    assert!(
        t["gaps"].to_string().contains("Mote could not be read"),
        "{t}"
    );
}

#[test]
fn role_send_uses_real_mote_lease_and_refuses_an_expired_holder() {
    let Some(p) = Project::new("roles") else {
        return;
    };
    let session = p.mote(
        "bob",
        &["--json", "session", "start", "--as", "bob", "--ttl", "1h"],
    );
    assert!(
        session.status.success(),
        "{}",
        String::from_utf8_lossy(&session.stderr)
    );
    let session: Value = serde_json::from_slice(&session.stdout).unwrap();
    let sid = session["session_id"].as_str().unwrap();
    let defined = p.mote(
        "alice",
        &[
            "role",
            "define",
            "reviewer",
            "--remit",
            "Review this fixture",
            "--assigner",
            "alice",
            "--idempotency-key",
            "fixture-role",
        ],
    );
    assert!(
        defined.status.success(),
        "{}",
        String::from_utf8_lossy(&defined.stderr)
    );
    let assigned = p.mote(
        "alice",
        &[
            "role",
            "assign",
            "reviewer",
            "bob",
            "--session",
            sid,
            "--ttl",
            "30m",
            "--idempotency-key",
            "fixture-assignment",
        ],
    );
    assert!(
        assigned.status.success(),
        "{}",
        String::from_utf8_lossy(&assigned.stderr)
    );
    let (ok, out) = p.fray(
        &[],
        "alice",
        &["--json", "send", "@role:reviewer", "Review the role route"],
    );
    assert!(ok, "{out}");
    let sent: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(sent["card"]["assignee"], "bob", "{sent}");
    assert!(
        sent["card"]["tags"].to_string().contains("role:reviewer"),
        "{sent}"
    );
    // Ending Mote's session invalidates its lease even though Fray is present.
    let ended = p.mote("bob", &["session", "end", sid]);
    assert!(
        ended.status.success(),
        "{}",
        String::from_utf8_lossy(&ended.stderr)
    );
    let (ok, out) = p.fray(
        &[],
        "alice",
        &["--json", "send", "@role:reviewer", "Must not queue"],
    );
    assert!(!ok && out.contains("no_live_role"), "{out}");
    let (_, out) = p.fray(&[], "alice", &["--json", "query", "--all"]);
    assert!(!out.contains("Must not queue"), "{out}");
}
