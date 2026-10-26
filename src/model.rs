//! Device records and identity reconciliation. No battery state or platform rules.
use periph_hal::{
    ComponentClaim, ComponentDescriptor, ComponentKey, ComponentRoute, Connection, CoverageScope,
    DeviceDescriptor, DeviceKey, EndpointKey, FailureScope, HardwareError, Identity, IdentityKind,
    ScopedFailure, Snapshot,
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
    pub components: Vec<SavedComponent>,
    pub alias: String,
    pub identity_conflict: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EndpointRoute {
    pub route: DeviceKey,
    pub endpoint: EndpointKey,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedComponent {
    /// Opaque persistent component ID; battery history refers to this string.
    pub id: String,
    pub routes: Vec<ComponentRoute>,
    pub parent: Option<String>,
    pub roles: Vec<String>,
    pub claims: Vec<ComponentClaim>,
    pub identity_conflict: bool,
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
    /// Route-specific HAL component keys bound to persistent component IDs.
    pub component_bindings: ComponentBindings,
    /// Confirmed component record remaps, scoped by owning device ID.
    pub merged_component_ids: Vec<ComponentIdRemap>,
}

pub type ComponentBindings = BTreeMap<ComponentRoute, String>;
/// `(owner device ID, old component ID, canonical component ID)`.
pub type ComponentIdRemap = (String, String, String);

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

fn observed_components(observation: &periph_hal::Device) -> Vec<ComponentDescriptor> {
    let mut components = observation.descriptor.components.clone();
    for capability in observation.capabilities.descriptors() {
        if !components
            .iter()
            .any(|component| component.key == capability.component)
        {
            components.push(ComponentDescriptor {
                key: capability.component.clone(),
                ..ComponentDescriptor::default()
            });
        }
    }
    components
}

fn merge_component_descriptor(target: &mut ComponentDescriptor, prior: &ComponentDescriptor) {
    if target.parent.is_none() {
        target.parent = prior.parent.clone();
    }
    for role in &prior.roles {
        if !target.roles.contains(role) {
            target.roles.push(role.clone());
        }
    }
    for claim in &prior.claims {
        if !target.claims.contains(claim) {
            target.claims.push(claim.clone());
        }
    }
}

fn component_claims_conflict(saved: &[ComponentClaim], incoming: &[ComponentClaim]) -> bool {
    saved.iter().any(|old| {
        incoming
            .iter()
            .any(|new| old.namespace == new.namespace && old.value != new.value)
    })
}

fn component_has_claim(saved: &SavedComponent, claims: &[ComponentClaim]) -> bool {
    claims.iter().any(|claim| saved.claims.contains(claim))
}

fn component_claim_id(claims: &[ComponentClaim]) -> String {
    let mut claims = claims.to_vec();
    claims.sort();
    claims.dedup();
    format!(
        "component:claim:{}",
        serde_json::to_string(&claims).unwrap()
    )
}

fn component_route_id(route: &ComponentRoute, claims: &[ComponentClaim]) -> String {
    let mut claims = claims.to_vec();
    claims.sort();
    claims.dedup();
    format!(
        "component:route:{}",
        serde_json::to_string(&(route, claims)).unwrap()
    )
}

fn merge_saved_component(target: &mut SavedComponent, absorbed: SavedComponent) {
    for route in absorbed.routes {
        if !target.routes.contains(&route) {
            target.routes.push(route);
        }
    }
    for role in absorbed.roles {
        if !target.roles.contains(&role) {
            target.roles.push(role);
        }
    }
    for claim in absorbed.claims {
        if !target.claims.contains(&claim) {
            target.claims.push(claim);
        }
    }
    if target.parent.is_none() {
        target.parent = absorbed.parent;
    }
    target.identity_conflict |= absorbed.identity_conflict;
}

fn canonical_component_id(device_id: &str, id: &str, merged: &[ComponentIdRemap]) -> String {
    let mut current = id.to_owned();
    for _ in 0..=merged.len() {
        let Some((_, _, next)) = merged
            .iter()
            .find(|(owner, old, _)| owner == device_id && old == &current)
        else {
            break;
        };
        if next == &current {
            break;
        }
        current = next.clone();
    }
    current
}

fn merge_components(
    devices: &mut [Device],
    snapshot: &Snapshot,
    device_bindings: &BTreeMap<DeviceKey, String>,
) -> (ComponentBindings, Vec<ComponentIdRemap>) {
    let mut claim_occupants =
        HashMap::<(String, DeviceKey, ComponentClaim), HashSet<ComponentKey>>::new();
    let mut ambiguous_claims = HashSet::<(String, ComponentClaim)>::new();
    let mut seen_observations = HashSet::new();
    for observation in &snapshot.devices {
        if !seen_observations.insert(observation.descriptor.key.clone()) {
            continue;
        }
        let Some(device_id) = device_bindings.get(&observation.descriptor.key) else {
            continue;
        };
        for component in observed_components(observation) {
            let unique_claims: HashSet<_> = component.claims.into_iter().collect();
            for claim in unique_claims {
                claim_occupants
                    .entry((device_id.clone(), observation.descriptor.key.clone(), claim))
                    .or_default()
                    .insert(component.key.clone());
            }
        }
    }
    for ((device_id, _, claim), components) in claim_occupants {
        if components.len() > 1 {
            ambiguous_claims.insert((device_id, claim));
        }
    }

    let mut component_bindings = BTreeMap::new();
    let mut merged_components = Vec::new();
    let mut pending_parents = Vec::<(String, ComponentRoute, Option<ComponentKey>)>::new();
    seen_observations.clear();
    for observation in &snapshot.devices {
        let route_key = &observation.descriptor.key;
        if !seen_observations.insert(route_key.clone()) {
            continue;
        }
        let Some(device_id) = device_bindings.get(route_key) else {
            continue;
        };
        let Some(device) = devices
            .iter_mut()
            .find(|device| device.saved.id == *device_id)
        else {
            continue;
        };
        for component in observed_components(observation) {
            let route = ComponentRoute {
                device: route_key.clone(),
                component: component.key.clone(),
            };
            let route_candidates: Vec<_> = device
                .saved
                .components
                .iter()
                .enumerate()
                .filter(|(_, saved)| {
                    saved.routes.contains(&route)
                        && !component_claims_conflict(&saved.claims, &component.claims)
                })
                .map(|(index, _)| index)
                .collect();
            let route_exists = device
                .saved
                .components
                .iter()
                .any(|saved| saved.routes.contains(&route));
            let source_route_match = if route_candidates.len() == 1 {
                route_candidates.first().copied()
            } else {
                None
            };
            let legacy_main = component.key == ComponentKey::legacy("main");
            let route_match = if legacy_main {
                if let Some(index) = device
                    .saved
                    .components
                    .iter()
                    .position(|saved| saved.id == "main")
                {
                    Some(index)
                } else if let Some(index) = source_route_match {
                    let old_id = device.saved.components[index].id.clone();
                    if old_id != "main" {
                        device.saved.components[index].id = "main".into();
                        merged_components.push((device_id.clone(), old_id, "main".into()));
                    }
                    Some(index)
                } else {
                    None
                }
            } else {
                source_route_match
            };

            let unique_claims: Vec<_> = component
                .claims
                .iter()
                .filter(|claim| {
                    !legacy_main
                        && !ambiguous_claims.contains(&(device_id.clone(), (*claim).clone()))
                })
                .collect();
            let mut claim_evidence = HashMap::<usize, HashSet<ComponentClaim>>::new();
            for claim in unique_claims {
                let mut matching_components = Vec::new();
                for (index, saved) in device.saved.components.iter().enumerate() {
                    if component_has_claim(saved, std::slice::from_ref(claim))
                        && !component_claims_conflict(&saved.claims, &component.claims)
                    {
                        matching_components.push(index);
                    }
                }
                if matching_components.len() == 1 {
                    claim_evidence
                        .entry(matching_components[0])
                        .or_default()
                        .insert((*claim).clone());
                }
            }
            let claim_candidates: HashSet<_> = claim_evidence.keys().copied().collect();
            let claim_match = (!legacy_main && claim_candidates.len() == 1)
                .then(|| *claim_candidates.iter().next().unwrap());
            let confirmed_merge_indices: Vec<_> = route_match
                .filter(|route_index| claim_evidence.contains_key(route_index))
                .map(|route_index| {
                    claim_evidence
                        .keys()
                        .copied()
                        .filter(|index| {
                            *index == route_index
                                || !component_claims_conflict(
                                    &device.saved.components[route_index].claims,
                                    &device.saved.components[*index].claims,
                                )
                        })
                        .collect()
                })
                .unwrap_or_default();

            let index = match (route_match, claim_match) {
                (Some(route_index), _) if confirmed_merge_indices.len() > 1 => {
                    let target_id = device.saved.components[route_index].id.clone();
                    let mut absorbed_indices: Vec<_> = confirmed_merge_indices
                        .into_iter()
                        .filter(|index| *index != route_index)
                        .collect();
                    absorbed_indices.sort_unstable_by(|left, right| right.cmp(left));
                    let mut target_index = route_index;
                    for absorbed_index in absorbed_indices {
                        let absorbed = device.saved.components.remove(absorbed_index);
                        if absorbed_index < target_index {
                            target_index -= 1;
                        }
                        if absorbed.id != target_id {
                            merged_components.push((
                                device_id.clone(),
                                absorbed.id.clone(),
                                target_id.clone(),
                            ));
                        }
                        merge_saved_component(&mut device.saved.components[target_index], absorbed);
                    }
                    target_index
                }
                (Some(route_index), Some(claim_index)) if route_index != claim_index => {
                    // A source route can be reused or can previously have had
                    // no component identity evidence. A strong claim to another
                    // saved component changes this route's binding, but does not
                    // prove that the old and new component histories are one.
                    device.saved.components[route_index]
                        .routes
                        .retain(|saved_route| saved_route != &route);
                    device.saved.components[route_index].identity_conflict = true;
                    device.saved.components[claim_index].identity_conflict = true;
                    claim_index
                }
                (Some(route_index), _) => route_index,
                (None, Some(claim_index)) => claim_index,
                (None, None) => {
                    let unique_claims: Vec<_> = component
                        .claims
                        .iter()
                        .filter(|claim| {
                            !legacy_main
                                && !ambiguous_claims
                                    .contains(&(device_id.clone(), (*claim).clone()))
                        })
                        .cloned()
                        .collect();
                    let id = if component.key == ComponentKey::legacy("main")
                        && !device
                            .saved
                            .components
                            .iter()
                            .any(|saved| saved.id == "main")
                    {
                        "main".to_owned()
                    } else if !unique_claims.is_empty()
                        && claim_candidates.is_empty()
                        && route_candidates.is_empty()
                    {
                        component_claim_id(&unique_claims)
                    } else {
                        component_route_id(&route, &component.claims)
                    };
                    let id = if device.saved.components.iter().any(|saved| saved.id == id) {
                        format!("{id}|route:{}", serde_json::to_string(&route).unwrap())
                    } else {
                        id
                    };
                    device.saved.components.push(SavedComponent {
                        id,
                        routes: vec![route.clone()],
                        parent: None,
                        roles: component.roles.clone(),
                        claims: component.claims.clone(),
                        identity_conflict: (route_exists && route_candidates.is_empty())
                            || claim_candidates.len() > 1,
                    });
                    device.saved.components.len() - 1
                }
            };

            let saved = &mut device.saved.components[index];
            if !saved.routes.contains(&route) {
                saved.routes.push(route.clone());
            }
            for role in &component.roles {
                if !saved.roles.contains(role) {
                    saved.roles.push(role.clone());
                }
            }
            for claim in &component.claims {
                if !saved.claims.contains(claim) {
                    saved.claims.push(claim.clone());
                }
            }
            pending_parents.push((device_id.clone(), route.clone(), component.parent.clone()));
            component_bindings.insert(route, saved.id.clone());
        }
    }

    for (device_id, route, parent_key) in pending_parents {
        let Some(parent_key) = parent_key else {
            continue;
        };
        let parent_route = ComponentRoute {
            device: route.device.clone(),
            component: parent_key,
        };
        let Some(parent_id) = component_bindings.get(&parent_route).cloned() else {
            continue;
        };
        if parent_id == component_bindings[&route] {
            continue;
        }
        if let Some(device) = devices
            .iter_mut()
            .find(|device| device.saved.id == device_id)
            && let Some(component) = device
                .saved
                .components
                .iter_mut()
                .find(|component| component.id == component_bindings[&route])
        {
            component.parent = Some(parent_id);
        }
    }

    for (route, id) in &mut component_bindings {
        if let Some(device_id) = device_bindings.get(&route.device) {
            *id = canonical_component_id(device_id, id, &merged_components);
        }
    }
    for device in devices {
        let device_id = device.saved.id.clone();
        for component in &mut device.saved.components {
            if let Some(parent) = component.parent.as_mut() {
                *parent = canonical_component_id(&device_id, parent, &merged_components);
            }
        }
    }
    (component_bindings, merged_components)
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
                let old_components = std::mem::take(&mut devices[other].saved.components);
                for component in old_components {
                    if let Some(existing) = devices[index]
                        .saved
                        .components
                        .iter_mut()
                        .find(|existing| existing.id == component.id)
                    {
                        merge_saved_component(existing, component);
                    } else {
                        devices[index].saved.components.push(component);
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
        updated.components = observed_components(observation);
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
            for prior in &device.saved.descriptor.components {
                if let Some(component) = updated
                    .components
                    .iter_mut()
                    .find(|component| component.key == prior.key)
                {
                    merge_component_descriptor(component, prior);
                } else {
                    updated.components.push(prior.clone());
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
    let (component_bindings, merged_component_ids) =
        merge_components(&mut devices, snapshot, &bindings);
    MergeResult {
        devices,
        bindings,
        merged_ids,
        component_bindings,
        merged_component_ids,
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

#[cfg(test)]
mod tests {
    use super::*;
    use periph_hal::{Capabilities, ComponentClaim, Device as HalDevice, DeviceDescriptor};

    fn component(key: &str, claim: Option<&str>) -> ComponentDescriptor {
        ComponentDescriptor {
            key: ComponentKey::new("fake.protocol.component", key),
            claims: claim
                .map(|value| {
                    vec![ComponentClaim {
                        namespace: "fake.protocol.component-uid".into(),
                        value: value.into(),
                    }]
                })
                .unwrap_or_default(),
            ..ComponentDescriptor::default()
        }
    }

    fn observation(backend: &str, local: &str, components: Vec<ComponentDescriptor>) -> HalDevice {
        observation_with_uid(backend, local, "headset-uid-1", components)
    }

    fn observation_with_uid(
        backend: &str,
        local: &str,
        uid: &str,
        components: Vec<ComponentDescriptor>,
    ) -> HalDevice {
        HalDevice {
            descriptor: DeviceDescriptor {
                key: DeviceKey {
                    backend: backend.into(),
                    local: local.into(),
                },
                name: "Synthetic headset".into(),
                identities: vec![Identity {
                    namespace: "fake.headset".into(),
                    kind: IdentityKind::UnitId,
                    subject: "device".into(),
                    value: uid.into(),
                }],
                components,
                ..DeviceDescriptor::default()
            },
            connection: Connection::Connected,
            capabilities: Capabilities::default(),
            issue: None,
        }
    }

    fn snapshot(devices: Vec<HalDevice>) -> Snapshot {
        Snapshot {
            devices,
            ..Snapshot::default()
        }
    }

    fn component_id(result: &MergeResult, device: &DeviceKey, key: &str) -> String {
        result
            .component_bindings
            .get(&ComponentRoute {
                device: device.clone(),
                component: if key == "main" {
                    ComponentKey::legacy(key)
                } else {
                    ComponentKey::new("fake.protocol.component", key)
                },
            })
            .unwrap()
            .clone()
    }

    #[test]
    fn legacy_main_is_the_shared_root_component_for_merged_sources() {
        let first_key = DeviceKey {
            backend: "fake.bluetooth".into(),
            local: "headset".into(),
        };
        let second_key = DeviceKey {
            backend: "fake.hid".into(),
            local: "receiver/slot-1".into(),
        };
        let result = merge_snapshot(
            &[],
            &snapshot(vec![
                observation(
                    "fake.bluetooth",
                    "headset",
                    vec![ComponentDescriptor {
                        key: ComponentKey::legacy("main"),
                        ..ComponentDescriptor::default()
                    }],
                ),
                observation(
                    "fake.hid",
                    "receiver/slot-1",
                    vec![ComponentDescriptor {
                        key: ComponentKey::legacy("main"),
                        ..ComponentDescriptor::default()
                    }],
                ),
            ]),
        );

        assert_eq!(component_id(&result, &first_key, "main"), "main");
        assert_eq!(component_id(&result, &second_key, "main"), "main");
        let saved = result
            .devices
            .iter()
            .find(|device| device.saved.id == result.bindings[&first_key])
            .unwrap();
        assert_eq!(
            saved
                .saved
                .components
                .iter()
                .filter(|component| component.id == "main")
                .count(),
            1
        );
    }

    #[test]
    fn unique_claims_bind_components_across_sources_and_reordering() {
        let left = component("gatt:bat-3", Some("cell-left-uid"));
        let right = component("gatt:bat-1", Some("cell-right-uid"));
        let case = component("gatt:bat-2", Some("case-uid"));
        let first = merge_snapshot(
            &[],
            &snapshot(vec![
                observation(
                    "fake.bluetooth",
                    "headset",
                    vec![left.clone(), right.clone(), case.clone()],
                ),
                observation(
                    "fake.hid",
                    "receiver/slot-1",
                    vec![
                        component("power-port:case", Some("case-uid")),
                        component("power-port:right", Some("cell-right-uid")),
                        component("power-port:left", Some("cell-left-uid")),
                    ],
                ),
            ]),
        );
        let bluetooth = DeviceKey {
            backend: "fake.bluetooth".into(),
            local: "headset".into(),
        };
        let hid = DeviceKey {
            backend: "fake.hid".into(),
            local: "receiver/slot-1".into(),
        };
        let stable_ids = [
            component_id(&first, &bluetooth, "gatt:bat-3"),
            component_id(&first, &bluetooth, "gatt:bat-1"),
            component_id(&first, &bluetooth, "gatt:bat-2"),
        ];
        assert_eq!(component_id(&first, &hid, "power-port:left"), stable_ids[0]);
        assert_eq!(
            component_id(&first, &hid, "power-port:right"),
            stable_ids[1]
        );
        assert_eq!(component_id(&first, &hid, "power-port:case"), stable_ids[2]);

        let reconnected = DeviceKey {
            backend: "fake.bluetooth".into(),
            local: "headset-session-2".into(),
        };
        let second = merge_snapshot(
            &first.devices,
            &snapshot(vec![observation(
                "fake.bluetooth",
                "headset-session-2",
                vec![
                    component("gatt:bat-2", Some("case-uid")),
                    component("gatt:bat-1", Some("cell-right-uid")),
                    component("gatt:bat-3", Some("cell-left-uid")),
                ],
            )]),
        );
        assert_eq!(
            component_id(&second, &reconnected, "gatt:bat-3"),
            stable_ids[0]
        );
        assert_eq!(
            component_id(&second, &reconnected, "gatt:bat-1"),
            stable_ids[1]
        );
        assert_eq!(
            component_id(&second, &reconnected, "gatt:bat-2"),
            stable_ids[2]
        );
    }

    #[test]
    fn missing_or_ambiguous_claims_do_not_merge_component_histories() {
        let no_claims = merge_snapshot(
            &[],
            &snapshot(vec![
                observation("fake.bluetooth", "a", vec![component("channel-a", None)]),
                observation("fake.hid", "b", vec![component("channel-b", None)]),
            ]),
        );
        assert_ne!(
            component_id(
                &no_claims,
                &DeviceKey {
                    backend: "fake.bluetooth".into(),
                    local: "a".into(),
                },
                "channel-a"
            ),
            component_id(
                &no_claims,
                &DeviceKey {
                    backend: "fake.hid".into(),
                    local: "b".into(),
                },
                "channel-b"
            )
        );

        let duplicate_claim = component("first", Some("duplicated-uid"));
        let ambiguous = merge_snapshot(
            &[],
            &snapshot(vec![observation(
                "fake.bluetooth",
                "one-device",
                vec![duplicate_claim, component("second", Some("duplicated-uid"))],
            )]),
        );
        assert_ne!(
            component_id(
                &ambiguous,
                &DeviceKey {
                    backend: "fake.bluetooth".into(),
                    local: "one-device".into(),
                },
                "first"
            ),
            component_id(
                &ambiguous,
                &DeviceKey {
                    backend: "fake.bluetooth".into(),
                    local: "one-device".into(),
                },
                "second"
            )
        );
    }

    #[test]
    fn a_new_strong_claim_takes_over_a_route_without_absorbing_old_history() {
        let first = merge_snapshot(
            &[],
            &snapshot(vec![
                observation("fake.slot", "slot-1", vec![component("battery", None)]),
                observation(
                    "fake.identity",
                    "stable-source",
                    vec![component("uid-route", Some("new-cell-uid"))],
                ),
            ]),
        );
        let slot_route = DeviceKey {
            backend: "fake.slot".into(),
            local: "slot-1".into(),
        };
        let identity_route = DeviceKey {
            backend: "fake.identity".into(),
            local: "stable-source".into(),
        };
        let old_history_id = component_id(&first, &slot_route, "battery");
        let trusted_id = component_id(&first, &identity_route, "uid-route");
        assert_ne!(old_history_id, trusted_id);

        let second = merge_snapshot(
            &first.devices,
            &snapshot(vec![observation(
                "fake.slot",
                "slot-1",
                vec![component("battery", Some("new-cell-uid"))],
            )]),
        );
        assert_eq!(component_id(&second, &slot_route, "battery"), trusted_id);
        assert!(!second.merged_component_ids.contains(&(
            second.bindings[&slot_route].clone(),
            old_history_id.clone(),
            trusted_id.clone()
        )));
        let device = second
            .devices
            .iter()
            .find(|device| device.saved.id == second.bindings[&slot_route])
            .unwrap();
        assert!(
            device
                .saved
                .components
                .iter()
                .any(|component| component.id == old_history_id)
        );
    }

    #[test]
    fn independent_unique_claims_merge_component_records_with_device_scope() {
        let first_a = DeviceKey {
            backend: "fake.bluetooth".into(),
            local: "device-a/session-a".into(),
        };
        let first_b = DeviceKey {
            backend: "fake.hid".into(),
            local: "device-a/receiver-slot".into(),
        };
        let second_a = DeviceKey {
            backend: "fake.bluetooth".into(),
            local: "device-b/session-a".into(),
        };
        let second_b = DeviceKey {
            backend: "fake.hid".into(),
            local: "device-b/receiver-slot".into(),
        };
        let claim_a = ComponentClaim {
            namespace: "test.component.uid-a".into(),
            value: "cell-a".into(),
        };
        let claim_b = ComponentClaim {
            namespace: "test.component.uid-b".into(),
            value: "cell-b".into(),
        };
        let first = merge_snapshot(
            &[],
            &snapshot(vec![
                observation_with_uid(
                    "fake.bluetooth",
                    &first_a.local,
                    "device-a-uid",
                    vec![ComponentDescriptor {
                        key: ComponentKey::new("fake.protocol.component", "first-route"),
                        claims: vec![claim_a.clone()],
                        ..ComponentDescriptor::default()
                    }],
                ),
                observation_with_uid(
                    "fake.hid",
                    &first_b.local,
                    "device-a-uid",
                    vec![ComponentDescriptor {
                        key: ComponentKey::new("fake.protocol.component", "second-route"),
                        claims: vec![claim_b.clone()],
                        ..ComponentDescriptor::default()
                    }],
                ),
                observation_with_uid(
                    "fake.bluetooth",
                    &second_a.local,
                    "device-b-uid",
                    vec![ComponentDescriptor {
                        key: ComponentKey::new("fake.protocol.component", "first-route"),
                        claims: vec![claim_a.clone()],
                        ..ComponentDescriptor::default()
                    }],
                ),
                observation_with_uid(
                    "fake.hid",
                    &second_b.local,
                    "device-b-uid",
                    vec![ComponentDescriptor {
                        key: ComponentKey::new("fake.protocol.component", "second-route"),
                        claims: vec![claim_b.clone()],
                        ..ComponentDescriptor::default()
                    }],
                ),
            ]),
        );
        let device_a_id = first.bindings[&first_a].clone();
        let device_b_id = first.bindings[&second_a].clone();
        assert_ne!(device_a_id, device_b_id);
        let component_a_id = component_id(&first, &first_a, "first-route");
        let component_b_id = component_id(&first, &first_b, "second-route");
        assert_eq!(
            component_a_id,
            component_id(&first, &second_a, "first-route")
        );
        assert_eq!(
            component_b_id,
            component_id(&first, &second_b, "second-route")
        );
        assert_ne!(component_a_id, component_b_id);

        let incoming = observation_with_uid(
            "fake.bluetooth",
            &first_a.local,
            "device-a-uid",
            vec![ComponentDescriptor {
                key: ComponentKey::new("fake.protocol.component", "first-route"),
                claims: vec![claim_a, claim_b],
                ..ComponentDescriptor::default()
            }],
        );
        let merged = merge_snapshot(&first.devices, &snapshot(vec![incoming]));
        assert!(merged.merged_component_ids.contains(&(
            device_a_id.clone(),
            component_b_id.clone(),
            component_a_id.clone()
        )));
        assert!(
            !merged
                .merged_component_ids
                .iter()
                .any(|(owner, _, _)| owner == &device_b_id)
        );
        assert_eq!(
            canonical_component_id(&device_b_id, &component_b_id, &merged.merged_component_ids),
            component_b_id
        );
    }
}
