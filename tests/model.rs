mod support;
use periph_hal::{BackendFailure, Connection, ErrorKind, HardwareError, IdentityKind};
use periph_in_one::model::{self, SavedDevice};
use support::{device, snapshot};

#[test]
fn generic_device_without_usb_ids_or_battery_has_a_stable_record() {
    let first = model::merge_snapshot(
        &[],
        &snapshot(vec![device("camera", "1", "", "")], &["camera"]),
    );
    assert!(first.devices[0].saved.descriptor.identifiers.is_empty());
    let second = model::merge_snapshot(
        &first.devices,
        &snapshot(vec![device("camera", "1", "", "")], &["camera"]),
    );
    assert_eq!(second.devices[0].saved.id, first.devices[0].saved.id);
    assert!(second.devices[0].present);
}

#[test]
fn strong_identity_follows_routes_and_preserves_missing_evidence() {
    let mut first = model::merge_snapshot(
        &[],
        &snapshot(vec![device("one", "old", "unit", "serial")], &["one"]),
    )
    .devices;
    first[0].saved.alias = "My device".into();
    let second = model::merge_snapshot(
        &first,
        &snapshot(vec![device("one", "new", "unit", "")], &["one"]),
    )
    .devices;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].saved.id, first[0].saved.id);
    assert_eq!(second[0].saved.alias, "My device");
    assert!(
        second[0]
            .saved
            .descriptor
            .identities
            .iter()
            .any(|identity| identity.value == "serial")
    );
    assert!(
        !second[0]
            .saved
            .routes
            .iter()
            .any(|route| route.local == "old")
    );
}

#[test]
fn conflicting_unit_ids_cannot_take_over_a_remark_on_the_same_route() {
    let mut first = model::merge_snapshot(
        &[],
        &snapshot(vec![device("one", "port", "A", "")], &["one"]),
    )
    .devices;
    first[0].saved.alias = "A remark".into();
    let second = model::merge_snapshot(
        &first,
        &snapshot(vec![device("one", "port", "B", "")], &["one"]),
    )
    .devices;
    assert_eq!(second.len(), 2);
    assert!(
        second
            .iter()
            .find(|record| record.present)
            .unwrap()
            .saved
            .alias
            .is_empty()
    );
    assert!(
        !second
            .iter()
            .find(|record| record.saved.alias == "A remark")
            .unwrap()
            .present
    );
}

#[test]
fn serial_collisions_are_remembered_across_partial_presence() {
    let mut first = model::merge_snapshot(
        &[],
        &snapshot(
            vec![
                device("one", "a", "", "same"),
                device("one", "b", "", "same"),
            ],
            &["one"],
        ),
    )
    .devices;
    assert_eq!(first.len(), 2);
    assert!(first.iter().all(|record| record.saved.identity_conflict));
    first[0].saved.alias = "Do not steal".into();
    let second = model::merge_snapshot(
        &first,
        &snapshot(vec![device("one", "c", "", "same")], &["one"]),
    )
    .devices;
    assert_eq!(second.len(), 3);
    assert!(
        second
            .iter()
            .find(|record| record.present)
            .unwrap()
            .saved
            .alias
            .is_empty()
    );
}

#[test]
fn different_identity_namespaces_and_device_names_do_not_imply_equality() {
    let a = device("one", "a", "1", "");
    let mut b = device("two", "b", "1", "");
    b.descriptor
        .identities
        .iter_mut()
        .find(|identity| identity.kind == IdentityKind::UnitId)
        .unwrap()
        .namespace = "another.vendor".into();
    let merged = model::merge_snapshot(&[], &snapshot(vec![a, b], &["one", "two"]));
    assert_eq!(merged.devices.len(), 2);
}

#[test]
fn receiver_slots_qualify_shared_receiver_evidence() {
    let slot = |number: u8| {
        let mut target = device("receiver", &format!("port#{number}"), "", "receiver-serial");
        target.descriptor.target = format!("slot:{number}");
        for identity in &mut target.descriptor.identities {
            identity.subject = "receiver".into();
            if identity.kind == IdentityKind::Location {
                identity.value = "port".into();
            }
        }
        target
    };
    let first =
        model::merge_snapshot(&[], &snapshot(vec![slot(1), slot(2)], &["receiver"])).devices;
    assert_eq!(first.len(), 2);
    assert!(first.iter().all(|record| !record.saved.identity_conflict));
    let id = first
        .iter()
        .find(|record| record.saved.descriptor.target == "slot:2")
        .unwrap()
        .saved
        .id
        .clone();
    let second = model::merge_snapshot(&first, &snapshot(vec![slot(2)], &["receiver"])).devices;
    assert_eq!(
        second
            .iter()
            .find(|record| record.present)
            .unwrap()
            .saved
            .id,
        id
    );
}

#[test]
fn reliable_identity_can_join_sources_and_keeps_independent_routes() {
    let first = model::merge_snapshot(
        &[],
        &snapshot(
            vec![
                device("one", "a", "shared", "shared-sn"),
                device("two", "b", "shared", "shared-sn"),
            ],
            &["one", "two"],
        ),
    );
    assert_eq!(first.devices.len(), 1);
    assert_eq!(first.bindings.len(), 2);
    assert_eq!(first.devices[0].saved.routes.len(), 2);
    assert!(!first.devices[0].saved.identity_conflict);
    let mut update = snapshot(vec![device("two", "b", "shared", "")], &["two"]);
    update.failures.push(BackendFailure {
        backend: "one".into(),
        error: HardwareError::new(ErrorKind::Transport, "source failure"),
    });
    let second = model::merge_snapshot(&first.devices, &update);
    assert_eq!(second.devices[0].connection, Connection::Connected);
}

#[test]
fn only_complete_coverage_can_mark_an_absent_device_offline() {
    let first =
        model::merge_snapshot(&[], &snapshot(vec![device("one", "a", "", "")], &["one"])).devices;
    let missing = model::merge_snapshot(&first, &snapshot(vec![], &[])).devices;
    assert!(missing[0].present);
    assert_eq!(missing[0].connection, Connection::Unknown);
    let offline = model::merge_snapshot(&first, &snapshot(vec![], &["one"])).devices;
    assert!(!offline[0].present);
    assert_eq!(offline[0].connection, Connection::Disconnected);
}

#[test]
fn conflicting_duplicate_remarks_survive_for_manual_rebinding() {
    let description = device("one", "a", "unit", "").descriptor;
    let saved = vec![
        SavedDevice {
            id: "old-a".into(),
            alias: "A".into(),
            descriptor: description.clone(),
            ..SavedDevice::default()
        },
        SavedDevice {
            id: "old-b".into(),
            alias: "B".into(),
            descriptor: description,
            ..SavedDevice::default()
        },
    ];
    let merged = model::merge_snapshot(
        &model::restore(&saved),
        &snapshot(vec![device("one", "a", "unit", "")], &["one"]),
    );
    assert_eq!(merged.devices.len(), 2);
    assert!(
        merged
            .devices
            .iter()
            .any(|record| record.saved.alias == "A")
    );
    assert!(
        merged
            .devices
            .iter()
            .any(|record| record.saved.alias == "B")
    );
}

#[test]
fn temporarily_missing_serial_on_the_same_route_keeps_the_record() {
    let first = model::merge_snapshot(
        &[],
        &snapshot(vec![device("one", "a", "", "serial")], &["one"]),
    )
    .devices;
    let second = model::merge_snapshot(
        &first,
        &snapshot(vec![device("one", "a", "", "")], &["one"]),
    )
    .devices;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].saved.id, first[0].saved.id);
}

#[test]
fn name_used_as_serial_is_not_a_unique_identity() {
    let mut a = device("one", "a", "", "Example device");
    let b = device("one", "b", "", "Example device");
    a.descriptor.name = "Example device".into();
    let merged = model::merge_snapshot(&[], &snapshot(vec![a, b], &["one"])).devices;
    assert_eq!(merged.len(), 2);
    assert!(merged.iter().all(|record| record.saved.identity_conflict));
}

#[test]
fn offline_deletion_and_explicit_remark_rebinding_keep_their_guards() {
    let mut records = model::merge_snapshot(
        &[],
        &snapshot(
            vec![device("one", "a", "A", ""), device("one", "b", "B", "")],
            &["one"],
        ),
    )
    .devices;
    let (a, b) = (records[0].saved.id.clone(), records[1].saved.id.clone());
    records[0].saved.alias = "Move me".into();
    assert!(model::remove_offline(&mut records, &a).is_err());
    assert!(model::rebind(&mut records, &a, &b).is_err());
    records[0].present = false;
    model::rebind(&mut records, &a, &b).unwrap();
    assert_eq!(records[1].saved.alias, "Move me");
    model::remove_offline(&mut records, &a).unwrap();
    assert_eq!(records.len(), 1);
}

#[test]
fn joined_sources_preserve_roles_and_uncertain_connection() {
    let mut camera = device("camera", "a", "shared", "");
    camera.connection = Connection::Unknown;
    camera.descriptor.roles = vec!["video.capture".into()];
    let mut audio = device("audio", "b", "shared", "");
    audio.connection = Connection::Disconnected;
    audio.descriptor.roles = vec!["audio.input".into()];
    let joined = model::merge_snapshot(&[], &snapshot(vec![camera, audio], &["camera", "audio"]));
    assert_eq!(joined.devices.len(), 1);
    assert!(
        joined.devices[0]
            .saved
            .descriptor
            .roles
            .iter()
            .any(|role| role == "video.capture")
    );
    assert!(
        joined.devices[0]
            .saved
            .descriptor
            .roles
            .iter()
            .any(|role| role == "audio.input")
    );
    assert_eq!(joined.devices[0].connection, Connection::Unknown);
}
