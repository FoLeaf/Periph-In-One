mod support;
use periph_in_one::{battery::Batteries, config::Config, hardware, model};
use serde_json::json;
use support::snapshot;

fn legacy() -> serde_json::Value {
    json!({"version":1,"width":740,"height":620,"devices":[{"id":"old-record","physical_id":"port-a","kind":"mouse","hardware_name":"Receiver","serial":"receiver-sn","uid":"unit-a","vid":14139,"pid":4630,"alias":"Work mouse","battery":80,"last_success":100}]})
}

#[test]
fn v1_migration_preserves_ids_remarks_history_and_exact_backup_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let bytes = serde_json::to_vec_pretty(&legacy()).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(config.version, 2);
    assert_eq!(config.devices[0].id, "old-record");
    assert_eq!(config.devices[0].alias, "Work mouse");
    assert_eq!(config.batteries[0].level, 80);
    assert_eq!(config.batteries[0].observed_at, 100);
    assert!(!path.with_extension("json.v1.bak").exists());
    config.save(&path).unwrap();
    assert_eq!(
        std::fs::read(path.with_extension("json.v1.bak")).unwrap(),
        bytes
    );
    let reloaded = Config::load(&path).unwrap();
    assert_eq!(reloaded.devices[0].descriptor, config.devices[0].descriptor);
    assert_eq!(reloaded.batteries[0].device_id, "old-record");
    config.save(&path).unwrap();
    assert_eq!(
        std::fs::read(path.with_extension("json.v1.bak")).unwrap(),
        bytes
    );
}

#[test]
fn generic_application_matches_migrated_evidence_without_legacy_key_parsing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    std::fs::write(&path, serde_json::to_vec(&legacy()).unwrap()).unwrap();
    let config = Config::load(&path).unwrap();
    let observation = periph_hal::Device {
        descriptor: config.devices[0].descriptor.clone(),
        connection: periph_hal::Connection::Connected,
        capabilities: Default::default(),
        issue: None,
    };
    let mut power = Batteries::restore(config.batteries.clone());
    let result = hardware::merge_report(
        &model::restore(&config.devices),
        &mut power,
        hardware::from_snapshot(snapshot(vec![observation], &["windows.hid"])),
        200,
    );
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].saved.id, "old-record");
    assert_eq!(result[0].saved.alias, "Work mouse");
    assert_eq!(power.view("old-record", "main").observed_at, Some(100));
}

#[test]
fn v1_single_receiver_slot_is_normalized_only_during_migration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let mut value = legacy();
    value["devices"][0]["vid"] = json!(0x046d);
    value["devices"][0]["pid"] = json!(0xc539);
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(config.devices[0].descriptor.key.local, "port-a#1");
    assert_eq!(config.devices[0].descriptor.target, "slot:1");
    assert_eq!(config.devices[0].id, "old-record");
}

#[test]
fn corrupt_or_unknown_configuration_is_never_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    for bytes in [b"{broken".as_slice(), b"{\"version\":999}".as_slice()] {
        std::fs::write(&path, bytes).unwrap();
        assert!(Config::load(&path).is_err());
        assert!(Config::default().save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn conflicting_existing_migration_backup_prevents_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let bytes = serde_json::to_vec(&legacy()).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let config = Config::load(&path).unwrap();
    std::fs::write(path.with_extension("json.v1.bak"), b"another original").unwrap();
    assert!(config.save(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn repeated_session_records_merge_history_without_losing_distinct_remarks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let mut value = legacy();
    value["devices"][0]["physical_id"] = json!("");
    value["devices"][0]["id"] = json!("local:[14139,4630,\"1-2:1:port-a\"]");
    let mut second = value["devices"][0].clone();
    second["id"] = json!("local:[14139,4630,\"3-4:1:port-a\"]");
    second["battery"] = json!(90);
    second["last_success"] = json!(200);
    value["devices"].as_array_mut().unwrap().push(second);
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let config = Config::load(&path).unwrap();
    let descriptor = config.devices[0].descriptor.clone();
    assert_eq!(descriptor.key.local, "port-a");
    let observation = periph_hal::Device {
        descriptor,
        connection: periph_hal::Connection::Connected,
        capabilities: Default::default(),
        issue: None,
    };
    let mut power = Batteries::restore(config.batteries);
    let result = hardware::merge_report(
        &model::restore(&config.devices),
        &mut power,
        hardware::from_snapshot(snapshot(vec![observation], &["windows.hid"])),
        300,
    );
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].saved.alias, "Work mouse");
    assert_eq!(power.view(&result[0].saved.id, "main").level, Some(90));
}
