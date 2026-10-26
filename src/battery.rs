//! Battery history belongs to this application domain, never to device records.
use periph_hal::{
    ComponentKey, ComponentRoute, Connection, DeviceKey,
    battery::{BatteryState, SampleOrigin},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedBattery {
    pub device_id: String,
    pub component: String,
    pub level: u8,
    pub observed_at: u64,
}

#[derive(Clone, Debug)]
pub struct Observation {
    pub key: DeviceKey,
    pub component: ComponentKey,
    pub state: BatteryState,
}

#[derive(Clone, Debug, Default)]
pub struct BatteryView {
    pub level: Option<u8>,
    pub observed_at: Option<u64>,
    pub current: bool,
    pub system_cached: bool,
    pub unsupported: bool,
    pub available: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Batteries {
    pub saved: Vec<SavedBattery>,
    current: BTreeMap<(String, String), BatteryView>,
    component_aliases: BTreeMap<(String, String), String>,
}

impl Batteries {
    pub fn restore(saved: Vec<SavedBattery>) -> Self {
        Self {
            saved,
            ..Self::default()
        }
    }

    pub fn apply(
        &mut self,
        observations: &[Observation],
        merge: &crate::model::MergeResult,
        now: u64,
    ) {
        for (old, new) in &merge.merged_ids {
            for record in &mut self.saved {
                if &record.device_id == old {
                    record.device_id = new.clone();
                }
            }
        }
        for (owner, old, new) in &merge.merged_component_ids {
            for record in &mut self.saved {
                if &record.device_id == owner && &record.component == old {
                    record.component = new.clone();
                }
            }
        }
        self.saved
            .sort_by_key(|record| std::cmp::Reverse(record.observed_at));
        let mut seen = std::collections::HashSet::new();
        self.saved
            .retain(|record| seen.insert((record.device_id.clone(), record.component.clone())));
        self.current.clear();
        self.component_aliases.clear();
        let mut ambiguous_aliases = HashSet::new();
        for observation in observations {
            let Some(id) = merge.bindings.get(&observation.key) else {
                continue;
            };
            let route = ComponentRoute {
                device: observation.key.clone(),
                component: observation.component.clone(),
            };
            let Some(component) = merge.component_bindings.get(&route) else {
                continue;
            };
            let component = canonical_component_id(id, component, &merge.merged_component_ids);
            let alias = component_alias(&observation.component);
            let alias_key = (id.clone(), alias);
            if !ambiguous_aliases.contains(&alias_key) {
                if let Some(existing) = self.component_aliases.get(&alias_key) {
                    if existing != &component {
                        self.component_aliases.remove(&alias_key);
                        ambiguous_aliases.insert(alias_key);
                    }
                } else {
                    self.component_aliases.insert(alias_key, component.clone());
                }
            }
            let connection = merge
                .devices
                .iter()
                .find(|device| &device.saved.id == id)
                .map(|device| device.connection);
            let mut view = BatteryView {
                available: true,
                ..BatteryView::default()
            };
            match &observation.state {
                BatteryState::Value(sample) if connection == Some(Connection::Connected) => {
                    let key = (id.clone(), component.clone());
                    if self.current.get(&key).is_some_and(|current| {
                        current.current
                            && (!current.system_cached
                                || sample.origin == SampleOrigin::SystemCache)
                    }) {
                        continue;
                    }
                    view.level = Some(sample.level.get());
                    view.current = true;
                    view.system_cached = sample.origin == SampleOrigin::SystemCache;
                    view.observed_at = Some(now);
                    self.saved.retain(|record| {
                        !(record.device_id == *id && record.component == component)
                    });
                    self.saved.push(SavedBattery {
                        device_id: id.clone(),
                        component: component.clone(),
                        level: sample.level.get(),
                        observed_at: now,
                    });
                }
                BatteryState::Unsupported => view.unsupported = true,
                _ => {}
            }
            let key = (id.clone(), component);
            // A later failed source must not erase another source's valid value.
            if view.current
                || !self
                    .current
                    .get(&key)
                    .is_some_and(|current| current.current)
            {
                self.current.insert(key, view);
            }
        }
    }

    pub fn view(&self, id: &str, component: &str) -> BatteryView {
        let component = self
            .component_aliases
            .get(&(id.into(), component.into()))
            .map(String::as_str)
            .unwrap_or(component);
        let mut view = self
            .current
            .get(&(id.into(), component.into()))
            .cloned()
            .unwrap_or_default();
        if view.level.is_none()
            && let Some(record) = self
                .saved
                .iter()
                .filter(|record| record.device_id == id && record.component == component)
                .max_by_key(|record| record.observed_at)
        {
            view.level = Some(record.level);
            view.observed_at = Some(record.observed_at);
        }
        view
    }

    pub fn remove(&mut self, id: &str) {
        self.saved.retain(|record| record.device_id != id);
        self.current.retain(|(device_id, _), _| device_id != id);
        self.component_aliases
            .retain(|(device_id, _), _| device_id != id);
    }
}

fn component_alias(key: &ComponentKey) -> String {
    if key.namespace == "legacy" {
        key.value.clone()
    } else {
        format!("{}:{}", key.namespace, key.value)
    }
}

fn canonical_component_id(
    id: &str,
    component: &str,
    merged: &[crate::model::ComponentIdRemap],
) -> String {
    let mut current = component.to_owned();
    for _ in 0..=merged.len() {
        let Some((_, _, next)) = merged
            .iter()
            .find(|(owner, old, _)| owner == id && old == &current)
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
