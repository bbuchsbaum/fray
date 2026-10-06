use fray::routing::choose;
use serde_json::json;
#[test]
fn mote_assignments_and_fray_liveness_both_gate_role_routing() {
    let policy = json!({"retired":null,"coverage":{"active_assignment_ids":["ra-a","ra-b"]},"assignments":[
        {"assignment_id":"ra-a","holder_actor":"alice","disposition":"active"},
        {"assignment_id":"ra-b","holder_actor":"bob","disposition":"active"},
        {"assignment_id":"ra-c","holder_actor":"carol","disposition":"expired"}]});
    let roster = json!({"items":[
        {"name":"alice","enabled":true,"reachability":"present","role":"worker"},
        {"name":"bob","enabled":true,"reachability":"wakeable","role":"worker"},
        {"name":"carol","enabled":true,"reachability":"wakeable","role":"reviewer"}],"more":false});
    let selected = choose("reviewer", Some(&policy), &roster, "author").unwrap();
    assert_eq!(selected["recipient"], "bob");
    assert_eq!(selected["assignment"], "ra-b");
    assert_eq!(
        choose("reviewer", Some(&policy), &roster, "bob").unwrap()["recipient"],
        "alice"
    );
    let empty =
        json!({"items":[{"name":"bob","enabled":true,"reachability":"absent"}],"more":false});
    assert_eq!(
        choose("reviewer", Some(&policy), &empty, "author")
            .unwrap_err()
            .code,
        "no_live_role"
    );
    let retired = json!({"retired":{"reason":"done"}});
    assert_eq!(
        choose("reviewer", Some(&retired), &roster, "author")
            .unwrap_err()
            .code,
        "role_retired"
    );
}
#[test]
fn without_mote_only_a_present_declared_role_holder_is_selected() {
    let roster = json!({"items":[{"name":"reviewer","role":"reviewer","enabled":true,"reachability":"present"}],"more":false});
    assert_eq!(
        choose("reviewer", None, &roster, "author").unwrap()["recipient"],
        "reviewer"
    );
    assert_eq!(
        choose("reviewer", None, &roster, "reviewer")
            .unwrap_err()
            .code,
        "no_live_role"
    );
    let incomplete = json!({"items":[],"more":true});
    assert_eq!(
        choose("reviewer", None, &incomplete, "author")
            .unwrap_err()
            .code,
        "role_roster_truncated"
    );
}
