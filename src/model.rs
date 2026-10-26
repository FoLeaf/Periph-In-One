//! Device records and identity reconciliation. No battery state or platform rules.
use periph_hal::{
    Connection, CoverageScope, DeviceDescriptor, DeviceKey, EndpointKey, FailureScope,
    HardwareError, Identity, IdentityKind, ScopedFailure, Snapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedDevice {
    /// Opaque persistent record ID; old IDs are preserved during migration.
    pub id: String,
    pub descriptor: DeviceDescriptor,
    pub routes: Vec<DeviceKey>,
    /// Access endpoint for each route; old config files infer the primary route
    /// from its descriptor and leave joined legacy routes unknown.
    pub endpoint_routes: Vec<EndpointRoute>,
    pub alias: String,
    pub identity_conflict: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EndpointRoute {
    pub route: DeviceKey,
    pub endpoint: EndpointKey,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub saved: SavedDevice,
    pub present: bool,
    pub connection: Connection,
    pub issue: Option<HardwareError>,
}

#[derive(Debug)]
pub struct MergeResult {
    pub devices: Vec<Device>,
    pub bindings: BTreeMap<DeviceKey, String>,
    /// Confirmed duplicate records; domains rebind their own history separately.
    pub merged_ids: Vec<(String, String)>,
}

pub fn restore(saved: &[SavedDevice]) -> Vec<Device> {
    saved
        .iter()
        .cloned()
        .map(|saved| Device {
            saved,
            present: false,
            connection: Connection::Disconnected,
            issue: None,
        })
        .collect()
}

fn serial_is_name(identity: &Identity, descriptor: &DeviceDescriptor) -> bool {
    identity.kind == IdentityKind::Serial
        && identity
            .value
            .trim()
            .eq_ignore_ascii_case(descriptor.name.trim())
}

fn claim_key(identity: &Identity, target: &str) -> String {
    let scope = if identity.subject == "device" {
        ""
    } else {
        target
    };
    format!(
        "identity:{}",
        serde_json::to_string(&(
            &identity.namespace,
            &identity.subject,
            scope,
            identity.kind,
            &identity.value
        ))
        .unwrap()
    )
}

fn strong(identity: &Identity) -> bool {
    matches!(
        identity.kind,
        IdentityKind::UnitId | IdentityKind::Serial | IdentityKind::Address
    )
}

fn contradictory(a: &DeviceDescriptor, b: &DeviceDescriptor) -> bool {
    a.identities
        .iter()
        .filter(|identity| {
            strong(identity) && !identity.value.is_empty() && !serial_is_name(identity, a)
        })
        .any(|left| {
            b.identities.iter().any(|right| {
                strong(right)
                    && !right.value.is_empty()
                    && !serial_is_name(right, b)
                    && left.kind == right.kind
                    && left.namespace == right.namespace
                    && left.subject == right.subject
                    && left.value != right.value
            })
        })
}

fn rank(kind: IdentityKind) -> u8 {
    match kind {
        IdentityKind::UnitId => 4,
        IdentityKind::Address => 3,
        IdentityKind::Serial => 2,
        IdentityKind::Location => 1,
        _ => 0,
    }
}

fn usable_claims(
    descriptor: &DeviceDescriptor,
    serial_conflict: bool,
) -> impl Iterator<Item = (&Identity, String)> {
    descriptor
        .identities
        .iter()
        .filter(move |identity| {
            !identity.value.is_empty()
                && !(identity.kind == IdentityKind::Serial
                    && (serial_conflict || serial_is_name(identity, descriptor)))
        })
        .map(|identity| (identity, claim_key(identity, &descriptor.target)))
}

fn routes(saved: &SavedDevice) -> impl Iterator<Item = &DeviceKey> {
    saved
        .routes
        .iter()
        .chain(std::iter::once(&saved.descriptor.key))
}

fn endpoint_for_descriptor(descriptor: &DeviceDescriptor) -> EndpointKey {
    if !descriptor.endpoint.is_empty() {
        return descriptor.endpoint.clone();
    }
    EndpointKey::for_device(&descriptor.key)
}

fn endpoint_for_route(saved: &SavedDevice, route: &DeviceKey) -> EndpointKey {
    saved
        .endpoint_routes
        .iter()
        .find(|mapping| &mapping.route == route)
        .map(|mapping| mapping.endpoint.clone())
        .unwrap_or_else(|| {
            if route == &saved.descriptor.key {
                endpoint_for_descriptor(&saved.descriptor)
            } else {
                EndpointKey::for_device(route)
            }
        })
}

fn scope_covers_route(scope: &CoverageScope, route: &DeviceKey, endpoint: &EndpointKey) -> bool {
    match scope {
        CoverageScope::Backend { backend } => backend == &route.backend,
        CoverageScope::Endpoint { endpoint: covered } => endpoint == covered,
        CoverageScope::Device { device } => device == route,
        CoverageScope::Component { .. } => false,
    }
}

fn failure_withholds_route(
    failure: &ScopedFailure,
    route: &DeviceKey,
    endpoint: &EndpointKey,
) -> bool {
    match &failure.scope {
        FailureScope::Backend { backend } => backend == &route.backend,
        FailureScope::Endpoint { endpoint: failed } => endpoint == failed,
        FailureScope::Device { device } => device == route,
        FailureScope::Component { .. } | FailureScope::Capability { .. } => false,
    }
}

fn route_covered(snapshot: &Snapshot, saved: &SavedDevice, route: &DeviceKey) -> bool {
    let endpoint = endpoint_for_route(saved, route);
    let covered = snapshot
        .coverage
        .iter()
        .any(|scope| scope_covers_route(scope, route, &endpoint))
        || (snapshot.coverage.is_empty()
            && snapshot
                .completed
                .iter()
                .any(|backend| backend == &route.backend));
    covered
        && !snapshot
            .scoped_failures
            .iter()
            .any(|failure| failure_withholds_route(failure, route, &endpoint))
        && !snapshot
            .failures
            .iter()
            .any(|failure| failure.backend == route.backend)
}

fn failure_for_saved_route<'a>(
    snapshot: &'a Snapshot,
    saved: &SavedDevice,
) -> Option<&'a HardwareError> {
    for route in routes(saved) {
        let endpoint = endpoint_for_route(saved, route);
        if let Some(failure) = snapshot
            .scoped_failures
            .iter()
            .find(|failure| failure_withholds_route(failure, route, &endpoint))
        {
            return Some(&failure.error);
        }
        if let Some(failure) = snapshot
            .failures
            .iter()
            .find(|failure| failure.backend == route.backend)
        {
            return Some(&failure.error);
        }
    }
    None
}

/// Reconcile facts from completed sources only. A missing/failed source leaves
/// its known devices unknown; it cannot authoritatively declare them offline.
pub fn merge_snapshot(previous: &[Device], snapshot: &Snapshot) -> MergeResult {
    let mut devices: Vec<_> = previous
        .iter()
        .map(|old| {
            let covered =
                routes(&old.saved).all(|route| route_covered(snapshot, &old.saved, route));
            Device {
                saved: old.saved.clone(),
                present: if covered { false } else { old.present },
                connection: if covered {
                    Connection::Disconnected
                } else {
                    Connection::Unknown
                },
                issue: failure_for_saved_route(snapshot, &old.saved).cloned(),
            }
        })
        .collect();
    let mut serial_counts = HashMap::<String, HashSet<String>>::new();
    for observation in &snapshot.devices {
        for identity in
            observation.descriptor.identities.iter().filter(|identity| {
                identity.kind == IdentityKind::Serial && !identity.value.is_empty()
            })
        {
            serial_counts
                .entry(claim_key(identity, &observation.descriptor.target))
                .or_default()
                .insert(
                    observation
                        .descriptor
                        .identities
                        .iter()
                        .find(|claim| {
                            matches!(claim.kind, IdentityKind::UnitId | IdentityKind::Address)
                                && !claim.value.is_empty()
                        })
                        .map(|claim| claim_key(claim, &observation.descriptor.target))
                        .unwrap_or_else(|| {
                            serde_json::to_string(&observation.descriptor.key).unwrap()
                        }),
                );
        }
    }
    let collisions: HashSet<_> = serial_counts
        .into_iter()
        .filter(|(_, keys)| keys.len() > 1)
        .map(|(key, _)| key)
        .collect();
    for device in &mut devices {
        if device.saved.descriptor.identities.iter().any(|identity| {
            identity.kind == IdentityKind::Serial
                && (serial_is_name(identity, &device.saved.descriptor)
                    || collisions.contains(&claim_key(identity, &device.saved.descriptor.target)))
        }) {
            device.saved.identity_conflict = true;
        }
    }
    let mut bindings = BTreeMap::new();
    let mut merged_ids = Vec::new();
    let mut seen = HashSet::new();
    for observation in &snapshot.devices {
        let descriptor = &observation.descriptor;
        if !seen.insert(descriptor.key.clone()) {
            continue;
        }
        let conflict = descriptor
            .identities
            .iter()
            .filter(|identity| identity.kind == IdentityKind::Serial)
            .any(|identity| {
                serial_is_name(identity, descriptor)
                    || collisions.contains(&claim_key(identity, &descriptor.target))
                    || devices.iter().any(|device| {
                        device.saved.identity_conflict
                            && usable_claims(&device.saved.descriptor, false)
                                .any(|(_, key)| key == claim_key(identity, &descriptor.target))
                    })
            });
        let incoming: Vec<_> = usable_claims(descriptor, conflict).collect();
        let mut candidates: Vec<_> = devices
            .iter()
            .enumerate()
            .filter_map(|(index, device)| {
                if contradictory(&device.saved.descriptor, descriptor) {
                    return None;
                }
                let matched =
                    usable_claims(&device.saved.descriptor, device.saved.identity_conflict)
                        .any(|(_, key)| incoming.iter().any(|(_, incoming)| *incoming == key));
                (matched || routes(&device.saved).any(|key| key == &descriptor.key))
                    .then_some(index)
            })
            .collect();
        let index = if candidates.is_empty() {
            let mut id = incoming
                .iter()
                .max_by_key(|(identity, _)| rank(identity.kind))
                .map(|(_, key)| key.clone())
                .unwrap_or_else(|| {
                    format!("route:{}", serde_json::to_string(&descriptor.key).unwrap())
                });
            if devices.iter().any(|device| device.saved.id == id) {
                id.push_str(&format!(
                    "|route:{}",
                    serde_json::to_string(&descriptor.key).unwrap()
                ));
            }
            devices.push(Device {
                saved: SavedDevice {
                    id,
                    descriptor: descriptor.clone(),
                    ..SavedDevice::default()
                },
                present: false,
                connection: Connection::Disconnected,
                issue: None,
            });
            devices.len() - 1
        } else {
            candidates.sort_by_key(|&index| {
                (
                    !routes(&devices[index].saved).any(|key| key == &descriptor.key),
                    devices[index].saved.alias.is_empty(),
                    index,
                )
            });
            let index = candidates[0];
            let alias = candidates
                .iter()
                .map(|&i| &devices[i].saved.alias)
                .find(|alias| !alias.is_empty())
                .cloned()
                .unwrap_or_default();
            if devices[index].saved.alias.is_empty() {
                devices[index].saved.alias = alias;
            }
            let mut remove = Vec::new();
            for &other in &candidates[1..] {
                if !devices[other].saved.alias.is_empty()
                    && devices[other].saved.alias != devices[index].saved.alias
                {
                    continue;
                }
                merged_ids.push((
                    devices[other].saved.id.clone(),
                    devices[index].saved.id.clone(),
                ));
                let old_routes = devices[other].saved.routes.clone();
                devices[index].saved.routes.extend(old_routes);
                let old_endpoint_routes = devices[other].saved.endpoint_routes.clone();
                for mapping in old_endpoint_routes {
                    if !devices[index]
                        .saved
                        .endpoint_routes
                        .iter()
                        .any(|existing| existing.route == mapping.route)
                    {
                        devices[index].saved.endpoint_routes.push(mapping);
                    }
                }
                remove.push(other);
            }
            let removed_before = remove.iter().filter(|&&other| other < index).count();
            remove.sort_unstable();
            for other in remove.into_iter().rev() {
                devices.remove(other);
            }
            index - removed_before
        };
        let device = &mut devices[index];
        let already_seen = bindings.values().any(|id| id == &device.saved.id);
        if !already_seen {
            let retained_routes: Vec<_> = device
                .saved
                .routes
                .iter()
                .filter(|route| !route_covered(snapshot, &device.saved, route))
                .cloned()
                .collect();
            let retained_set: HashSet<_> = retained_routes.iter().cloned().collect();
            device.saved.routes = retained_routes;
            device
                .saved
                .endpoint_routes
                .retain(|mapping| retained_set.contains(&mapping.route));
        }
        if !device.saved.routes.contains(&descriptor.key) {
            device.saved.routes.push(descriptor.key.clone());
        }
        let endpoint = endpoint_for_descriptor(descriptor);
        if let Some(mapping) = device
            .saved
            .endpoint_routes
            .iter_mut()
            .find(|mapping| mapping.route == descriptor.key)
        {
            mapping.endpoint = endpoint;
        } else {
            device.saved.endpoint_routes.push(EndpointRoute {
                route: descriptor.key.clone(),
                endpoint,
            });
        }
        let mut updated = descriptor.clone();
        if already_seen {
            for role in &device.saved.descriptor.roles {
                if !updated.roles.contains(role) {
                    updated.roles.push(role.clone());
                }
            }
            for identifier in &device.saved.descriptor.identifiers {
                if !updated.identifiers.contains(identifier) {
                    updated.identifiers.push(identifier.clone());
                }
            }
            for link in &device.saved.descriptor.links {
                if !updated.links.contains(link) {
                    updated.links.push(link.clone());
                }
            }
        }
        // Temporarily missing strong evidence must not downgrade identity.
        for identity in &device.saved.descriptor.identities {
            if strong(identity) && !updated.identities.contains(identity) {
                updated.identities.push(identity.clone());
            }
        }
        if updated.name.is_empty() {
            updated.name = device.saved.descriptor.name.clone();
        }
        device.saved.descriptor = updated;
        device.saved.identity_conflict |= conflict;
        device.present |= observation.connection != Connection::Disconnected;
        device.connection = match (
            if already_seen || device.connection == Connection::Unknown {
                device.connection
            } else {
                Connection::Disconnected
            },
            observation.connection,
        ) {
            (Connection::Connected, _) | (_, Connection::Connected) => Connection::Connected,
            (Connection::Unknown, _) | (_, Connection::Unknown) => Connection::Unknown,
            _ => Connection::Disconnected,
        };
        device.issue = if device.connection == Connection::Connected {
            None
        } else {
            observation.issue.clone()
        };
        bindings.insert(descriptor.key.clone(), device.saved.id.clone());
    }
    // Earlier bindings may point at a duplicate absorbed later in this round.
    for id in bindings.values_mut() {
        for (old, new) in &merged_ids {
            if id == old {
                *id = new.clone();
            }
        }
    }
    MergeResult {
        devices,
        bindings,
        merged_ids,
    }
}

pub fn remove_offline(devices: &mut Vec<Device>, id: &str) -> Result<(), &'static str> {
    let index = devices
        .iter()
        .position(|device| device.saved.id == id)
        .ok_or("设备记录已不存在")?;
    if devices[index].present {
        return Err("设备已连接，无法删除");
    }
    devices.remove(index);
    Ok(())
}

pub fn rebind(devices: &mut [Device], from: &str, to: &str) -> Result<(), &'static str> {
    if from == to {
        return Err("请选择不同的设备记录");
    }
    let source = devices
        .iter()
        .position(|device| device.saved.id == from && !device.present)
        .ok_or("原记录必须是离线设备")?;
    let target = devices
        .iter()
        .position(|device| device.saved.id == to && device.present)
        .ok_or("目标设备必须在线")?;
    if devices[source].saved.alias.is_empty() {
        return Err("原记录没有备注");
    }
    if !devices[target].saved.alias.is_empty() {
        return Err("请先清空目标设备的备注");
    }
    devices[target].saved.alias = std::mem::take(&mut devices[source].saved.alias);
    Ok(())
}
