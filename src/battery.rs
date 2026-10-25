//! Battery history belongs to this application domain, never to device records.
use periph_hal::{
    Connection, DeviceKey,
    battery::{BatteryState, SampleOrigin},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    pub component: String,
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
        self.saved
            .sort_by_key(|record| std::cmp::Reverse(record.observed_at));
        let mut seen = std::collections::HashSet::new();
        self.saved
            .retain(|record| seen.insert((record.device_id.clone(), record.component.clone())));
        self.current.clear();
        for observation in observations {
            let Some(id) = merge.bindings.get(&observation.key) else {
                continue;
            };
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
                    let key = (id.clone(), observation.component.clone());
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
                        !(record.device_id == *id && record.component == observation.component)
                    });
                    self.saved.push(SavedBattery {
                        device_id: id.clone(),
                        component: observation.component.clone(),
                        level: sample.level.get(),
                        observed_at: now,
                    });
                }
                BatteryState::Unsupported => view.unsupported = true,
                _ => {}
            }
            let key = (id.clone(), observation.component.clone());
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
    }
}
