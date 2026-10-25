mod support;
use periph_hal::{
    Connection,
    battery::{BatteryState, SampleOrigin},
};
use periph_in_one::{battery::Batteries, hardware};
use support::{battery, device, snapshot};

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
    uncertain
        .capabilities
        .insert("main", BatteryState::Value(sample))
        .unwrap();
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
    target
        .capabilities
        .insert("main", BatteryState::Unsupported)
        .unwrap();
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
