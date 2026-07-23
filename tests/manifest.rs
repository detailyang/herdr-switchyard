use std::fs;

#[test]
fn plugin_manifest_declares_the_popup_action_and_sync_hook() {
    let input = fs::read_to_string("herdr-plugin.toml").unwrap();
    let manifest: toml::Value = toml::from_str(&input).unwrap();

    assert_eq!(manifest["id"].as_str(), Some("herdr.switchyard"));
    assert_eq!(manifest["min_herdr_version"].as_str(), Some("0.7.5"));
    assert_eq!(manifest["actions"][0]["id"].as_str(), Some("open"));
    let events = manifest["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["on"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events, ["pane.agent_detected", "pane.agent_status_changed"]);
    assert_eq!(manifest["panes"][0]["id"].as_str(), Some("picker"));
    assert_eq!(manifest["panes"][0]["placement"].as_str(), Some("popup"));
    assert_eq!(
        manifest["panes"][0]["command"][0].as_str(),
        Some("./target/release/herdr-switchyard")
    );
}
