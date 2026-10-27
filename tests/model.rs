mod support;
use periph_hal::{
    BackendFailure, Connection, CoverageScope, EndpointKey, ErrorKind, FailureScope, HardwareError,
    IdentityKind, OperationKind, ScopedFailure, Snapshot,
};
use periph_in_one::model::{self, SavedDevice};
use support::{device, gamepad, snapshot};

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
fn failed_receiver_endpoint_withholds_its_slots_but_other_endpoints_go_offline() {
    let endpoint = EndpointKey {
        backend: "windows.hid".into(),
        local: "receiver-container-a".into(),
    };
    let other_endpoint = EndpointKey {
        backend: "windows.hid".into(),
        local: "receiver-container-b".into(),
    };
    let slot = |endpoint: &EndpointKey, local: &str, target: &str| {
        let mut device = device("windows.hid", local, "", "");
        device.descriptor.endpoint = endpoint.clone();
        device.descriptor.target = target.into();
        device
    };
    let first = model::merge_snapshot(
        &[],
        &snapshot(
            vec![
                slot(&endpoint, "port-a#1", "slot:1"),
                slot(&endpoint, "port-a#2", "slot:2"),
                slot(&other_endpoint, "port-b#1", "slot:1"),
            ],
            &["windows.hid"],
        ),
    )
    .devices;

    let partial = Snapshot {
        completed: vec!["windows.hid".into()],
        coverage: vec![CoverageScope::Backend {
            backend: "windows.hid".into(),
        }],
        scoped_failures: vec![ScopedFailure {
            scope: FailureScope::Endpoint {
                endpoint: endpoint.clone(),
            },
            operation: OperationKind::Discovery,
            error: HardwareError::new(ErrorKind::Timeout, "receiver stopped responding"),
        }],
        ..Snapshot::default()
    };
    let second = model::merge_snapshot(&first, &partial);
    for local in ["port-a#1", "port-a#2"] {
        let old = second
            .devices
            .iter()
            .find(|record| record.saved.routes.iter().any(|route| route.local == local))
            .unwrap();
        assert!(old.present);
        assert_eq!(old.connection, Connection::Unknown);
        assert_eq!(old.issue.as_ref().unwrap().kind, ErrorKind::Timeout);
    }
    let other = second
        .devices
        .iter()
        .find(|record| {
            record
                .saved
                .routes
                .iter()
                .any(|route| route.local == "port-b#1")
        })
        .unwrap();
    assert!(!other.present);
    assert_eq!(other.connection, Connection::Disconnected);
}

#[test]
fn capability_failure_does_not_replace_confirmed_connection_or_withhold_device_coverage() {
    let current = device("one", "a", "unit", "");
    let previous = model::merge_snapshot(&[], &snapshot(vec![current.clone()], &["one"])).devices;
    let report = Snapshot {
        devices: vec![current],
        completed: vec!["one".into()],
        coverage: vec![CoverageScope::Backend {
            backend: "one".into(),
        }],
        scoped_failures: vec![ScopedFailure {
            scope: FailureScope::Capability {
                device: periph_hal::DeviceKey {
                    backend: "one".into(),
                    local: "a".into(),
                },
                component: "main".into(),
                id: "power.battery".into(),
                version: 2,
            },
            operation: OperationKind::Capability,
            error: HardwareError::new(ErrorKind::Timeout, "battery read timed out"),
        }],
        ..Snapshot::default()
    };
    let result = model::merge_snapshot(&previous, &report);
    assert!(result.devices[0].present);
    assert_eq!(result.devices[0].connection, Connection::Connected);
    assert!(result.devices[0].issue.is_none());
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

#[test]
fn idle_and_active_gamepad_on_the_same_port_keep_the_record() {
    let active = gamepad("port-1", 0x2104, "423bca92863cba8d");
    let mut first = model::merge_snapshot(&[], &snapshot(vec![active], &["windows.hid"])).devices;
    assert_eq!(first.len(), 1);
    assert!(first[0].saved.id.starts_with("identity:[\"usb:413d\""));
    first[0].saved.alias = "客厅手柄".into();

    // The idle endpoint is present but provides no controller connection proof.
    let docked = gamepad("port-1", 0x2106, "");
    let second = model::merge_snapshot(&first, &snapshot(vec![docked], &["windows.hid"])).devices;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].saved.alias, "客厅手柄");
    assert!(second[0].present);
    assert_eq!(second[0].saved.id, first[0].saved.id);
    assert_eq!(second[0].connection, Connection::Unknown);
    assert_eq!(
        second[0].saved.descriptor.name,
        first[0].saved.descriptor.name
    );
    assert_eq!(second[0].saved.descriptor.identifiers[0].product, 0x2106);
    assert!(
        second[0]
            .saved
            .descriptor
            .identities
            .iter()
            .any(|id| id.value == "423bca92863cba8d")
    );

    // Active again:
    let active_again = gamepad("port-1", 0x2104, "423bca92863cba8d");
    let third =
        model::merge_snapshot(&second, &snapshot(vec![active_again], &["windows.hid"])).devices;
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].saved.alias, "客厅手柄");
}

#[test]
fn earlier_idle_gamepad_promotes_id_when_uid_arrives() {
    let docked = gamepad("port-1", 0x2106, "");
    let mut first = model::merge_snapshot(&[], &snapshot(vec![docked], &["windows.hid"])).devices;
    assert_eq!(first.len(), 1);
    assert!(
        first[0]
            .saved
            .id
            .starts_with("identity:[\"windows.usb-port\"")
    );
    first[0].saved.alias = "客厅手柄".into();

    // Active personality arrives with hardware UID:
    let active = gamepad("port-1", 0x2104, "423bca92863cba8d");
    let second = model::merge_snapshot(&first, &snapshot(vec![active], &["windows.hid"])).devices;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].saved.alias, "客厅手柄");
    assert!(second[0].saved.id.starts_with("identity:[\"usb:413d\""));
    assert_eq!(second[0].saved.descriptor.name, "XBOX 360 For Windows");
}

#[test]
fn simultaneous_gamepad_endpoints_without_shared_identity_stay_separate() {
    let active_dongle = gamepad("port-a", 0x2104, "423bca92863cba8d");
    let charging_dock = gamepad("port-b", 0x2106, "");
    let result = model::merge_snapshot(
        &[],
        &snapshot(vec![active_dongle, charging_dock], &["windows.hid"]),
    );
    assert_eq!(result.devices.len(), 2);
    assert!(result.devices.iter().all(|device| device.present));
    let bound_ids: std::collections::BTreeSet<_> = result.bindings.values().collect();
    assert_eq!(bound_ids.len(), 2);
    assert!(result.merged_ids.is_empty());
}

#[test]
fn unidentified_gamepad_on_another_port_does_not_take_over_a_remark() {
    let active = gamepad("port-a", 0x2104, "423bca92863cba8d");
    let mut first = model::merge_snapshot(&[], &snapshot(vec![active], &["windows.hid"])).devices;
    assert_eq!(first.len(), 1);
    first[0].saved.alias = "客厅手柄".into();

    // No evidence associates this idle endpoint with the previous controller.
    let docked = gamepad("port-b", 0x2106, "");
    let second = model::merge_snapshot(&first, &snapshot(vec![docked], &["windows.hid"])).devices;
    assert_eq!(second.len(), 2);
    assert!(!second[0].present);
    assert_eq!(second[0].saved.id, first[0].saved.id);
    assert_eq!(second[0].saved.alias, "客厅手柄");
    assert!(second[1].present);
    assert!(second[1].saved.alias.is_empty());
}

#[test]
fn different_gamepads_with_distinct_uids_stay_separate() {
    let first = gamepad("port-1", 0x2104, "423bca92863cba8d");
    let second = gamepad("port-2", 0x2104, "aabbccddeeff0011");
    let result = model::merge_snapshot(&[], &snapshot(vec![first, second], &["windows.hid"]));
    assert_eq!(result.devices.len(), 2);
    assert_ne!(result.devices[0].saved.id, result.devices[1].saved.id);
}

#[test]
fn idle_gamepad_endpoints_on_different_ports_stay_separate() {
    let result = model::merge_snapshot(
        &[],
        &snapshot(
            vec![gamepad("port-a", 0x2106, ""), gamepad("port-b", 0x2106, "")],
            &["windows.hid"],
        ),
    );
    assert_eq!(result.devices.len(), 2);
    assert!(result.merged_ids.is_empty());
}

#[test]
fn idle_endpoint_cannot_bridge_distinct_uids_in_any_enumeration_order() {
    let observations = [
        gamepad("port-a", 0x2104, "423bca92863cba8d"),
        gamepad("port-b", 0x2104, "aabbccddeeff0011"),
        gamepad("port-c", 0x2106, ""),
    ];
    let expected = model::merge_snapshot(&[], &snapshot(observations.to_vec(), &["windows.hid"]));
    assert_eq!(expected.devices.len(), 3);
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let scan = snapshot(
            order.map(|index| observations[index].clone()).to_vec(),
            &["windows.hid"],
        );
        let result = model::merge_snapshot(&[], &scan);
        assert_eq!(result.devices.len(), 3, "order={order:?}");
        assert_eq!(result.bindings, expected.bindings, "order={order:?}");
        assert!(result.merged_ids.is_empty());
        let repeated = model::merge_snapshot(&result.devices, &scan);
        assert_eq!(repeated.bindings, result.bindings);
        assert_eq!(repeated.devices.len(), 3);
    }
}

#[test]
fn shared_uuid_joins_wired_and_wireless_endpoints_and_follows_ports() {
    let uid = "423bca92863cba8d";
    for reverse in [false, true] {
        let mut observations = vec![
            gamepad("port-a", 0x2104, uid),
            gamepad("port-b", 0x2114, uid),
        ];
        if reverse {
            observations.reverse();
        }
        let mut first = model::merge_snapshot(&[], &snapshot(observations, &["windows.hid"]));
        assert_eq!(first.devices.len(), 1);
        assert_eq!(first.devices[0].saved.routes.len(), 2);
        first.devices[0].saved.alias = "客厅手柄".into();
        let second = model::merge_snapshot(
            &first.devices,
            &snapshot(vec![gamepad("port-c", 0x2114, uid)], &["windows.hid"]),
        );
        assert_eq!(second.devices.len(), 1);
        assert_eq!(second.devices[0].saved.id, first.devices[0].saved.id);
        assert_eq!(second.devices[0].saved.alias, "客厅手柄");
    }
}

#[test]
fn weak_observation_does_not_choose_between_conflicting_historical_route_owners() {
    let first = model::merge_snapshot(
        &[],
        &snapshot(vec![device("one", "shared", "A", "")], &["one"]),
    );
    let second = model::merge_snapshot(
        &first.devices,
        &snapshot(vec![device("one", "shared", "B", "")], &["one"]),
    );
    assert_eq!(second.devices.len(), 2);
    let mut previous = second.devices;
    previous[0].saved.alias = "A 的备注".into();
    previous[1].saved.alias = "B 的备注".into();
    let mut weak = device("one", "shared", "", "");
    weak.connection = Connection::Unknown;
    let scan = snapshot(vec![weak], &["one"]);
    let ambiguous = model::merge_snapshot(&previous, &scan);
    assert_eq!(ambiguous.devices.len(), 3);
    assert!(ambiguous.merged_ids.is_empty());
    let weak_id = ambiguous.bindings.values().next().unwrap();
    assert!(
        ambiguous
            .devices
            .iter()
            .find(|device| &device.saved.id == weak_id)
            .unwrap()
            .saved
            .alias
            .is_empty()
    );

    let saved: Vec<_> = ambiguous
        .devices
        .iter()
        .map(|device| device.saved.clone())
        .collect();
    let repeated = model::merge_snapshot(&model::restore(&saved), &scan);
    assert_eq!(repeated.devices.len(), 3);
    assert_eq!(repeated.bindings, ambiguous.bindings);
    assert!(repeated.merged_ids.is_empty());

    // Strong evidence can resolve the temporary route record later.
    let resolved = model::merge_snapshot(
        &repeated.devices,
        &snapshot(vec![device("one", "shared", "A", "")], &["one"]),
    );
    assert_eq!(resolved.devices.len(), 2);
    assert!(
        resolved
            .devices
            .iter()
            .any(|device| device.saved.alias == "A 的备注")
    );
    assert!(
        resolved
            .devices
            .iter()
            .any(|device| device.saved.alias == "B 的备注")
    );
    assert_eq!(
        resolved.merged_ids,
        vec![(weak_id.clone(), previous[0].saved.id.clone())]
    );
}
