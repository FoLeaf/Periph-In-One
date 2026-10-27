mod support;
use periph_hal::{
    ComponentClaim, ComponentDescriptor, ComponentKey, Connection,
    battery::{BatteryState, SampleOrigin},
};
use periph_in_one::{battery::Batteries, hardware};
use support::{battery, device, gamepad, snapshot};

#[test]
fn idle_gamepad_observation_preserves_independent_battery_histories() {
    let mut power = Batteries::default();
    let mut a = gamepad("port-a", 0x2104, "423bca92863cba8d");
    let mut b = gamepad("port-b", 0x2104, "aabbccddeeff0011");
    battery(&mut a, "main", Some(30));
    battery(&mut b, "main", Some(80));
    let previous = hardware::merge_report(
        &[],
        &mut power,
        hardware::from_snapshot(snapshot(vec![a, b], &["windows.hid"])),
        10,
    );
    let mut idle = gamepad("port-c", 0x2106, "");
    battery(&mut idle, "main", None);
    let current = hardware::merge_report(
        &previous,
        &mut power,
        hardware::from_snapshot(snapshot(vec![idle], &["windows.hid"])),
        20,
    );
    assert_eq!(current.len(), 3);
    assert_eq!(power.saved.len(), 2);
    for (old, expected) in previous.iter().zip([30, 80]) {
        let view = power.view(&old.saved.id, "main");
        assert_eq!(view.level, Some(expected));
        assert_eq!(view.observed_at, Some(10));
        assert!(!view.current);
    }
}

#[test]
fn sleeping_and_waking_on_the_same_port_keep_remark_and_battery_age() {
    let mut power = Batteries::default();
    let mut active = gamepad("port-a", 0x2104, "423bca92863cba8d");
    battery(&mut active, "main", Some(80));
    let mut first = hardware::merge_report(
        &[],
        &mut power,
        hardware::from_snapshot(snapshot(vec![active], &["windows.hid"])),
        10,
    );
    first[0].saved.alias = "客厅手柄".into();
    let id = first[0].saved.id.clone();
    let mut idle = gamepad("port-a", 0x2106, "");
    battery(&mut idle, "main", None);
    let sleeping = hardware::merge_report(
        &first,
        &mut power,
        hardware::from_snapshot(snapshot(vec![idle], &["windows.hid"])),
        20,
    );
    assert_eq!(sleeping.len(), 1);
    assert_eq!(sleeping[0].saved.id, id);
    assert_eq!(sleeping[0].connection, Connection::Unknown);
    assert_eq!(sleeping[0].saved.alias, "客厅手柄");
    assert_eq!(periph_in_one::presentation::status(&sleeping[0]), "未响应");
    let historical = power.view(&id, "main");
    assert_eq!(historical.level, Some(80));
    assert_eq!(historical.observed_at, Some(10));
    assert!(!historical.current);

    let mut awake = gamepad("port-a", 0x2104, "423bca92863cba8d");
    battery(&mut awake, "main", Some(75));
    let resumed = hardware::merge_report(
        &sleeping,
        &mut power,
        hardware::from_snapshot(snapshot(vec![awake], &["windows.hid"])),
        30,
    );
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].saved.id, id);
    assert_eq!(resumed[0].saved.alias, "客厅手柄");
    assert_eq!(resumed[0].connection, Connection::Connected);
    let current = power.view(&id, "main");
    assert_eq!(current.level, Some(75));
    assert_eq!(current.observed_at, Some(30));
    assert!(current.current);
    assert_eq!(power.saved.len(), 1);
}

#[test]
fn missing_reading_does_not_refresh_history_or_change_connection() {
    let mut power = Batteries::default();
    let mut first = device("one", "a", "unit", "");
    battery(&mut first, "main", Some(80));
    let records = hardware::merge_report(
        &[],
        &mut power,
        hardware::from_snapshot(snapshot(vec![first], &["one"])),
        10,
    );
    let id = records[0].saved.id.clone();
    let mut second = device("one", "a", "unit", "");
    battery(&mut second, "main", None);
    let records = hardware::merge_report(
        &records,
        &mut power,
        hardware::from_snapshot(snapshot(vec![second], &["one"])),
        20,
    );
    assert_eq!(records[0].connection, Connection::Connected);
    let view = power.view(&id, "main");
    assert_eq!(view.level, Some(80));
    assert_eq!(view.observed_at, Some(10));
    assert!(!view.current);
    assert!(!view.unsupported);
}

#[test]
fn no_battery_is_required_and_multiple_components_are_independent() {
    let mut power = Batteries::default();
    let camera = device("camera", "a", "camera", "");
    let mut headset = device("audio", "b", "headset", "");
    battery(&mut headset, "left", Some(0));
    battery(&mut headset, "right", Some(70));
    let records = hardware::merge_report(
        &[],
        &mut power,
        hardware::from_snapshot(snapshot(vec![camera, headset], &["camera", "audio"])),
        1,
    );
    assert_eq!(records.len(), 2);
    assert!(!power.view(&records[0].saved.id, "main").available);
    assert_eq!(power.view(&records[1].saved.id, "left").level, Some(0));
    assert_eq!(power.view(&records[1].saved.id, "right").level, Some(70));
}

#[test]
fn cache_without_connection_proof_is_never_a_current_reading() {
    let mut power = Batteries::default();
    let mut uncertain = device("one", "a", "unit", "");
    uncertain.connection = Connection::Unknown;
    let sample = periph_hal::battery::Sample {
        level: periph_hal::battery::Percent::new(80).unwrap(),
        origin: SampleOrigin::SystemCache,
        observed_at: std::time::UNIX_EPOCH,
        sampled_at: None,
    };
    support::battery_state(&mut uncertain, "main", BatteryState::Value(sample));
    let records = hardware::merge_report(
        &[],
        &mut power,
        hardware::from_snapshot(snapshot(vec![uncertain], &["one"])),
        1,
    );
    assert_eq!(records[0].connection, Connection::Unknown);
    assert!(!power.view(&records[0].saved.id, "main").current);
    assert!(power.saved.is_empty());
}

#[test]
fn unsupported_and_offline_history_do_not_look_fresh() {
    let mut power = Batteries::default();
    let mut target = device("one", "a", "unit", "");
    support::battery_state(&mut target, "main", BatteryState::Unsupported);
    let records = hardware::merge_report(
        &[],
        &mut power,
        hardware::from_snapshot(snapshot(vec![target], &["one"])),
        1,
    );
    let view = power.view(&records[0].saved.id, "main");
    assert!(view.unsupported);
    assert!(!view.current);
    assert_eq!(view.level, None);
}

#[test]
fn direct_reading_wins_over_cache_in_both_view_and_history() {
    for reversed in [false, true] {
        let mut power = Batteries::default();
        let mut cached = device("cache", "a", "unit", "");
        let sample = periph_hal::battery::Sample {
            level: periph_hal::battery::Percent::new(20).unwrap(),
            origin: SampleOrigin::SystemCache,
            observed_at: std::time::UNIX_EPOCH,
            sampled_at: None,
        };
        support::battery_state(&mut cached, "main", BatteryState::Value(sample));
        let mut direct = device("direct", "b", "unit", "");
        battery(&mut direct, "main", Some(80));
        let observations = if reversed {
            vec![direct, cached]
        } else {
            vec![cached, direct]
        };
        let records = hardware::merge_report(
            &[],
            &mut power,
            hardware::from_snapshot(snapshot(observations, &["cache", "direct"])),
            1,
        );
        let id = &records[0].saved.id;
        assert_eq!(power.view(id, "main").level, Some(80));
        assert!(!power.view(id, "main").system_cached);
        let offline = hardware::merge_report(
            &records,
            &mut power,
            hardware::from_snapshot(snapshot(vec![], &["cache", "direct"])),
            2,
        );
        assert_eq!(power.view(&offline[0].saved.id, "main").level, Some(80));
        assert!(!power.view(&offline[0].saved.id, "main").current);
    }
}

#[test]
fn component_history_uses_persistent_binding_across_reconnection() {
    let mut power = Batteries::default();
    let mut first = device("bluetooth", "session-1", "headset-uid", "");
    battery(&mut first, "left", Some(40));
    first.descriptor.components.push(ComponentDescriptor {
        key: ComponentKey::legacy("left"),
        claims: vec![ComponentClaim {
            namespace: "test.component.uid".into(),
            value: "left-cell-uid".into(),
        }],
        ..ComponentDescriptor::default()
    });
    let records = hardware::merge_report(
        &[],
        &mut power,
        hardware::from_snapshot(snapshot(vec![first], &["bluetooth"])),
        10,
    );
    let record_id = records[0].saved.id.clone();
    let first_history = power
        .saved
        .iter()
        .find(|record| record.device_id == record_id)
        .unwrap()
        .component
        .clone();
    assert_ne!(first_history, "left");
    assert_eq!(power.view(&record_id, "left").level, Some(40));

    let mut reconnected = device("bluetooth", "session-2", "headset-uid", "");
    battery(&mut reconnected, "left", Some(58));
    reconnected.descriptor.components.push(ComponentDescriptor {
        key: ComponentKey::legacy("left"),
        claims: vec![ComponentClaim {
            namespace: "test.component.uid".into(),
            value: "left-cell-uid".into(),
        }],
        ..ComponentDescriptor::default()
    });
    let records = hardware::merge_report(
        &records,
        &mut power,
        hardware::from_snapshot(snapshot(vec![reconnected], &["bluetooth"])),
        20,
    );
    assert_eq!(records[0].saved.id, record_id);
    let history: Vec<_> = power
        .saved
        .iter()
        .filter(|record| record.device_id == record_id)
        .collect();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].component, first_history);
    assert_eq!(history[0].level, 58);
    assert_eq!(power.view(&record_id, "left").level, Some(58));
}

#[test]
fn component_id_remaps_are_scoped_to_the_owning_device() {
    let mut power = Batteries::default();
    power.saved = vec![
        periph_in_one::battery::SavedBattery {
            device_id: "device-a".into(),
            component: "shared-id".into(),
            level: 40,
            observed_at: 10,
        },
        periph_in_one::battery::SavedBattery {
            device_id: "device-b".into(),
            component: "shared-id".into(),
            level: 70,
            observed_at: 20,
        },
    ];
    let merge = periph_in_one::model::MergeResult {
        devices: Vec::new(),
        bindings: Default::default(),
        merged_ids: Vec::new(),
        component_bindings: Default::default(),
        merged_component_ids: vec![("device-a".into(), "shared-id".into(), "main".into())],
    };

    power.apply(&[], &merge, 30);

    assert!(
        power
            .saved
            .iter()
            .any(|record| record.device_id == "device-a" && record.component == "main")
    );
    assert!(
        power
            .saved
            .iter()
            .any(|record| record.device_id == "device-b" && record.component == "shared-id")
    );
    assert_eq!(power.saved.len(), 2);
}
