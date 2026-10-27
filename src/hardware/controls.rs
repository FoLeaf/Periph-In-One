//! Optional controller reads coordinated by the refresh deadline.
use super::record_failure;
use periph_hal::{
    Capability, ComponentKey, Snapshot, dpi::Dpi, lighting::Lighting, operation::OperationRequest,
};
use std::time::Instant;
enum ControlRead {
    Dpi(OperationRequest<Option<u8>>),
    Lighting(OperationRequest<bool>),
}

enum ControlValue {
    Dpi(Option<u8>),
    Lighting(bool),
}

pub(super) struct PendingControl {
    key: periph_hal::DeviceKey,
    read: ControlRead,
}

pub(super) fn queue(devices: &[periph_hal::Device]) -> Vec<PendingControl> {
    let mut pending = Vec::new();
    for device in devices
        .iter()
        .filter(|device| device.connection != periph_hal::Connection::Disconnected)
    {
        if let Some(dpi) = device.capabilities.get::<Dpi>("main") {
            pending.push(PendingControl {
                key: device.descriptor.key.clone(),
                read: ControlRead::Dpi(dpi.request_read()),
            });
        }
        if let Some(lighting) = device.capabilities.get::<Lighting>("main") {
            pending.push(PendingControl {
                key: device.descriptor.key.clone(),
                read: ControlRead::Lighting(lighting.request_read()),
            });
        }
    }
    pending
}

pub(super) fn poll(
    pending: &mut Vec<PendingControl>,
    snapshot: &mut Snapshot,
    dpi_stages: &mut Vec<(periph_hal::DeviceKey, Option<u8>)>,
    lighting_states: &mut Vec<(periph_hal::DeviceKey, bool)>,
    deadline: Instant,
) -> bool {
    let mut changed = false;
    pending.retain(|operation| {
        let (result, id, version) = match &operation.read {
            ControlRead::Dpi(request) => (
                request
                    .try_take()
                    .map(|result| result.map(ControlValue::Dpi)),
                Dpi::ID,
                Dpi::VERSION,
            ),
            ControlRead::Lighting(request) => (
                request
                    .try_take()
                    .map(|result| result.map(ControlValue::Lighting)),
                Lighting::ID,
                Lighting::VERSION,
            ),
        };
        let result = result.or_else(|| {
            (Instant::now() >= deadline).then(|| {
                Err(periph_hal::HardwareError::new(
                    periph_hal::ErrorKind::Timeout,
                    "control read exceeded refresh deadline",
                ))
            })
        });
        let Some(result) = result else {
            return true;
        };
        match result {
            Ok(ControlValue::Dpi(stage)) => dpi_stages.push((operation.key.clone(), stage)),
            Ok(ControlValue::Lighting(on)) => lighting_states.push((operation.key.clone(), on)),
            Err(error) => record_failure(
                snapshot,
                &operation.key,
                &ComponentKey::legacy("main"),
                id,
                version,
                &error,
            ),
        }
        changed = true;
        false
    });
    changed
}
