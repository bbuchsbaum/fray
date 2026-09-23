use fray::diagnostics::listening;
use serde_json::json;

#[test]
fn transport_presence_does_not_imply_host_activation_or_work() {
    let connected =
        json!({"enabled":true,"listener":{"live":true,"state":"armed","expires_ms":900}});
    let report = listening(&connected, 100);
    assert_eq!(report["live"], true);
    assert_eq!(report["activation"], "unknown");
    assert_eq!(report["model_response_guaranteed"], false);
    let mut declared = connected.clone();
    declared["listener"]["activation"] = json!({"mode":"native-monitor","expires_ms":200});
    assert_eq!(listening(&declared, 100)["activation_expired"], false);
    assert_eq!(listening(&declared, 200)["activation_expired"], true);
    assert_eq!(listening(&declared, 200)["live"], true);
    declared["enabled"] = json!(false);
    assert_eq!(listening(&declared, 100)["live"], false);
}

#[test]
fn stale_and_managed_presence_remain_distinct() {
    let mut agent = json!({"enabled":true,"listener":{"live":false,"state":"stale"},"controller":{"live":false,"state":"stale"}});
    assert_eq!(listening(&agent, 0)["live"], false);
    agent["controller"] = json!({"live":true,"state":"waiting"});
    let status = listening(&agent, 0);
    assert_eq!(status["activation"], "managed");
    assert_eq!(status["state"], "waiting");
    assert_eq!(status["model_response_guaranteed"], false);
}
