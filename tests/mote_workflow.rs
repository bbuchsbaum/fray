use fray::{
    model::{Request, Result},
    store::Store,
};
use serde_json::{json, Value};
const NOW: i64 = 1_800_000_000_000;
fn run(s: &mut Store, actor: &str, op: &str, args: Value) -> Result<Value> {
    s.execute_at(&Request::new(op, actor, args), NOW)
}
fn call(s: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    run(s, actor, op, args).unwrap()
}
fn candidate(id: &str, commit: char) -> Value {
    json!({"candidate_id":id,"entity":"work","proposer":"writer","identity":{"store_id":"st-test","commit_oid":commit.to_string().repeat(40),"base_oid":"a".repeat(40)},"phase":{"value":"pending","op_id":"phase"},"policy":{"reviewers":["reader"],"authorizer":"owner"},"reviews":{},"supersession":{"successor_id":null}})
}
fn fixture() -> (Store, i64, Value) {
    let mut s = Store::memory().unwrap();
    for actor in ["writer", "reader", "other"] {
        call(&mut s, actor, "join", json!({"topics":[]}));
    }
    call(
        &mut s,
        "writer",
        "mote_bind",
        json!({"store":"/tmp/.mote","store_id":"st-test"}),
    );
    let review = call(
        &mut s,
        "writer",
        "mote_review_request",
        json!({"to":"reader","title":"Review","body":"Evidence","candidate_view":candidate("mc-one",'b'),"store_id":"st-test"}),
    );
    let id = review["card"]["id"].as_i64().unwrap();
    let payload = json!({"store_id":"st-test","cwd":"/tmp/work","card_id":id,"subject_rev":1,"at":format!("git:{}","b".repeat(40)),"baseline":format!("git:{}","a".repeat(40)),"candidate_id":"mc-one","verdict":"object","body":"Fix input validation","argv":["candidate","review","mc-one","block","--idempotency-key","key"],"evidence":[],"role":null});
    (s, id, payload)
}
#[test]
fn immutable_mote_subject_refuses_advisory_verdict_and_sha_movement() {
    let (mut s, id, _) = fixture();
    for (op, args) in [
        (
            "review_subject",
            json!({"id":id,"expect":1,"at":format!("git:{}","c".repeat(40))}),
        ),
        (
            "annotate",
            json!({"id":id,"body":"approve","kind":"evidence","review_verdict":{"verdict":"approve","at":format!("git:{}","b".repeat(40)),"expect":1}}),
        ),
    ] {
        assert!(run(&mut s, "writer", op, args).is_err());
    }
    let shown = call(&mut s, "reader", "show", json!({"id":id,"history":true}));
    assert_eq!(shown["review"]["advisory"], false);
    assert!(shown["review"]["verdicts"].as_array().unwrap().is_empty());
}
#[test]
fn exact_journal_freezes_tokens_and_rejection_records_no_verdict() {
    let (mut s, id, payload) = fixture();
    let op = call(
        &mut s,
        "reader",
        "mote_operation_prepare",
        json!({"key":"review-key","kind":"review","payload":payload}),
    );
    assert_eq!(op["operation"]["rev"], 0);
    let mut changed = payload.clone();
    changed["argv"] = json!(["different-token"]);
    assert_eq!(
        run(
            &mut s,
            "reader",
            "mote_operation_prepare",
            json!({"key":"review-key","kind":"review","payload":changed})
        )
        .unwrap_err()
        .code,
        "idempotency_conflict"
    );
    let failure = json!({"exit_code":2,"data":{"outcome":"conflict","git_updated":null,"git_updated_unknown":true,"journal":"kept"}});
    let saved = call(
        &mut s,
        "reader",
        "mote_operation_checkpoint",
        json!({"key":"review-key","expect":0,"step":"review_attempt","observation":failure}),
    );
    assert_eq!(
        saved["operation"]["observations"][0]["observation"],
        failure
    );
    assert!(run(
        &mut s,
        "reader",
        "mote_operation_finalize",
        json!({"key":"review-key","expect":1,"result":{"confirmed":false,"receipt":failure}})
    )
    .is_err());
    let shown = call(&mut s, "reader", "show", json!({"id":id,"history":true}));
    assert!(shown["review"]["verdicts"].as_array().unwrap().is_empty());
    assert_eq!(
        call(
            &mut s,
            "reader",
            "mote_operation_get",
            json!({"key":"review-key"})
        )["operation"]["state"],
        "pending"
    );
    assert!(run(
        &mut s,
        "other",
        "mote_operation_get",
        json!({"key":"review-key"})
    )
    .is_err());
}
#[test]
fn accepted_receipt_finalizes_once_and_verified_successor_carries_open_objection() {
    let (mut s, id, payload) = fixture();
    call(
        &mut s,
        "reader",
        "mote_operation_prepare",
        json!({"key":"review-key","kind":"review","payload":payload}),
    );
    let result = json!({"confirmed":true,"receipt":{"exit_code":0,"data":candidate("mc-one",'b')},"current_review_matches":true});
    let done = call(
        &mut s,
        "reader",
        "mote_operation_finalize",
        json!({"key":"review-key","expect":0,"result":result}),
    );
    let event = done["operation"]["result"]["fray"]["event_seq"].clone();
    let repeated = call(
        &mut s,
        "reader",
        "mote_operation_finalize",
        json!({"key":"review-key","expect":0,"result":result}),
    );
    assert_eq!(repeated["operation"]["result"]["fray"]["event_seq"], event);
    let mut old = candidate("mc-one", 'b');
    old["phase"]["value"] = json!("superseded");
    old["supersession"]["successor_id"] = json!("mc-two");
    let wrong = run(
        &mut s,
        "writer",
        "mote_review_successor",
        json!({"id":id,"expect":1,"predecessor_view":old,"candidate_view":candidate("mc-wrong",'c')}),
    );
    assert!(wrong.is_err());
    let next = call(
        &mut s,
        "writer",
        "mote_review_successor",
        json!({"id":id,"expect":1,"predecessor_view":old,"candidate_view":candidate("mc-two",'c')}),
    );
    assert_eq!(next["carried_objections"].as_array().unwrap().len(), 1);
    let new_id = next["successor"]["card"]["id"].clone();
    let new = call(
        &mut s,
        "reader",
        "show",
        json!({"id":new_id,"history":true}),
    );
    assert_eq!(new["review"]["mote_candidate"]["candidate_id"], "mc-two");
    assert!(new["review"]["verdicts"].as_array().unwrap().is_empty());
    assert_eq!(new["follow_ups"].as_array().unwrap().len(), 1);
    assert_eq!(new["objections"]["items"][0]["objector"], "reader");
    let carried_id = next["carried_objections"][0].clone();
    assert_eq!(
        run(
            &mut s,
            "writer",
            "patch",
            json!({"id":carried_id,"expect":1,"status":"resolved"})
        )
        .unwrap_err()
        .code,
        "objection_authority"
    );
    assert_eq!(
        run(
            &mut s,
            "writer",
            "patch",
            json!({"id":new_id,"expect":1,"status":"resolved"})
        )
        .unwrap_err()
        .code,
        "open_objections"
    );
    let original = call(&mut s, "reader", "show", json!({"id":id,"history":true}));
    assert_eq!(original["review"]["verdicts"][0]["stale"], true);
    assert_eq!(original["follow_ups"][0]["status"], "open");
}
#[test]
fn self_review_and_bad_subject_fail_before_a_journal_is_written() {
    let (mut s, _, mut payload) = fixture();
    assert_eq!(
        run(
            &mut s,
            "writer",
            "mote_operation_prepare",
            json!({"key":"self","kind":"review","payload":payload})
        )
        .unwrap_err()
        .code,
        "self_review"
    );
    payload["at"] = json!(format!("git:{}", "c".repeat(40)));
    assert_eq!(
        run(
            &mut s,
            "reader",
            "mote_operation_prepare",
            json!({"key":"stale","kind":"review","payload":payload})
        )
        .unwrap_err()
        .code,
        "conflict"
    );
    assert!(run(
        &mut s,
        "reader",
        "mote_operation_get",
        json!({"key":"stale"})
    )
    .is_err());
}

#[test]
fn durable_journal_survives_reopen_and_client_lock_releases_without_unlinking() {
    use std::{fs, path::PathBuf};
    let root = PathBuf::from("/tmp").join(format!(
        "fray-journal-{}",
        fray::model::random_key().unwrap()
    ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("board.db");
    let payload =
        json!({"store_id":"st-persist","cwd":root,"argv":["claim","issue","--ttl","300"]});
    {
        let mut s = Store::open(&path, false).unwrap();
        call(&mut s, "reader", "join", json!({"topics":[]}));
        call(
            &mut s,
            "reader",
            "mote_bind",
            json!({"store":"/tmp/.mote","store_id":"st-persist"}),
        );
        call(
            &mut s,
            "reader",
            "mote_operation_prepare",
            json!({"key":"key","kind":"accept","payload":payload}),
        );
        call(
            &mut s,
            "reader",
            "mote_operation_checkpoint",
            json!({"key":"key","expect":0,"step":"unknown","observation":{"timeout":true}}),
        );
    }
    {
        let mut s = Store::open(&path, false).unwrap();
        let restored =
            call(&mut s, "reader", "mote_operation_get", json!({"key":"key"}))["operation"].clone();
        assert_eq!(restored["payload"], payload);
        assert_eq!(restored["rev"], 1);
        assert_eq!(restored["state"], "pending");
        assert_eq!(
            restored["digest"],
            fray::mote_workflow::digest(&payload).unwrap()
        );
    }
    let context = fray::mote_workflow::Context {
        home: &root,
        actor: "reader",
        store: fray::mote::Store {
            path: root.join(".mote"),
            store_id: "st-persist".into(),
        },
        cwd: root.clone(),
    };
    let guard = context.lock("key").unwrap();
    assert_eq!(context.lock("key").unwrap_err().code, "operation_busy");
    drop(guard);
    let guard = context.lock("key").unwrap();
    drop(guard);
    assert_eq!(
        fs::read_dir(root.join("operation-locks")).unwrap().count(),
        1
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn historical_acceptance_never_replaces_a_current_block_or_creates_a_new_blocking_followup() {
    let (mut s, id, payload) = fixture();
    call(
        &mut s,
        "reader",
        "mote_operation_prepare",
        json!({"key":"block","kind":"review","payload":payload}),
    );
    call(
        &mut s,
        "reader",
        "mote_operation_finalize",
        json!({"key":"block","expect":0,"result":{"confirmed":true,"receipt":{"exit_code":0},"current_review_matches":true}}),
    );
    for (key, verdict) in [("old-approve", "approve"), ("old-object", "object")] {
        let mut historical = payload.clone();
        historical["verdict"] = json!(verdict);
        call(
            &mut s,
            "reader",
            "mote_operation_prepare",
            json!({"key":key,"kind":"review","payload":historical}),
        );
        let done = call(
            &mut s,
            "reader",
            "mote_operation_finalize",
            json!({"key":key,"expect":0,"result":{"confirmed":true,"receipt":{"exit_code":0},"current_review_matches":false}}),
        );
        assert!(done["operation"]["result"]["fray"]["follow_up"].is_null());
    }
    let shown = call(&mut s, "reader", "show", json!({"id":id,"history":true}));
    assert_eq!(shown["review"]["latest_verdict"]["verdict"], "object");
    assert_eq!(shown["review"]["verdicts"].as_array().unwrap().len(), 1);
    assert_eq!(shown["follow_ups"].as_array().unwrap().len(), 1);
}
