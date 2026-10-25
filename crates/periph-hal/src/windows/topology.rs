//! Windows devnode properties and receiver topology. No protocol queries.
use super::wide;
use hidapi::DeviceInfo;
use windows_sys::Win32::{
    Devices::{
        DeviceAndDriverInstallation::*,
        HumanInterfaceDevice::HidD_GetSerialNumberString,
        Properties::{DEVPKEY_Device_ContainerId, DEVPKEY_Device_Parent},
    },
    Foundation::{CloseHandle, DEVPROPKEY, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
};

pub(super) fn raw_serial_number(info: &DeviceInfo) -> String {
    // hidapi fills absent USB/Bluetooth serials from PnP IDs. Query the HID
    // descriptor directly so a topology/path ID is never presented as an SN.
    unsafe {
        let handle = CreateFileW(
            wide(&info.path().to_string_lossy()).as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return String::new();
        }
        let mut buffer = [0u16; 256];
        let valid = HidD_GetSerialNumberString(
            handle,
            buffer.as_mut_ptr().cast(),
            std::mem::size_of_val(&buffer) as u32,
        );
        CloseHandle(handle);
        if !valid {
            return String::new();
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end]).trim().into()
    }
}

/// Run `probe` against a fresh device-info set that is always destroyed.
fn with_device_set<T>(probe: impl FnOnce(HDEVINFO) -> Option<T>) -> Option<T> {
    unsafe {
        let set = SetupDiCreateDeviceInfoList(std::ptr::null(), std::ptr::null_mut());
        if set == -1 {
            return None;
        }
        let result = probe(set);
        SetupDiDestroyDeviceInfoList(set);
        result
    }
}

/// Read one device property from a devnode already opened in `set`, into
/// `buffer`, returning the property size in bytes.
fn node_property(
    set: HDEVINFO,
    device: &SP_DEVINFO_DATA,
    key: &DEVPROPKEY,
    buffer: &mut [u8],
) -> Option<usize> {
    let mut kind = 0;
    let mut size = 0;
    (unsafe {
        SetupDiGetDevicePropertyW(
            set,
            device,
            key,
            &mut kind,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut size,
            0,
        )
    } != 0)
        .then_some(size as usize)
}

/// Resolve the devnode behind a HID interface and read one device property.
fn device_property(info: &DeviceInfo, key: &DEVPROPKEY, buffer: &mut [u8]) -> Option<usize> {
    with_device_set(|set| unsafe {
        let mut interface = SP_DEVICE_INTERFACE_DATA {
            cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..std::mem::zeroed()
        };
        if SetupDiOpenDeviceInterfaceW(
            set,
            wide(&info.path().to_string_lossy()).as_ptr(),
            0,
            &mut interface,
        ) == 0
        {
            return None;
        }
        let mut needed = 0;
        SetupDiGetDeviceInterfaceDetailW(
            set,
            &interface,
            std::ptr::null_mut(),
            0,
            &mut needed,
            std::ptr::null_mut(),
        );
        if needed == 0 {
            return None;
        }
        // Aligned storage for the variable-size structure.
        let mut detail = vec![0u64; (needed as usize).div_ceil(8)];
        let detail_ptr = detail
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        (*detail_ptr).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        let mut device = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..std::mem::zeroed()
        };
        if SetupDiGetDeviceInterfaceDetailW(
            set,
            &interface,
            detail_ptr,
            needed,
            std::ptr::null_mut(),
            &mut device,
        ) == 0
        {
            return None;
        }
        node_property(set, &device, key, buffer)
    })
}

/// Windows groups all interfaces of a physical receiver using its ContainerId.
/// This is an internal local identifier, never displayed as a hardware serial.
pub(crate) fn container_id(info: &DeviceInfo) -> String {
    let mut value = [0u8; 16];
    let read = device_property(info, &DEVPKEY_Device_ContainerId, &mut value).unwrap_or(0);
    if read == value.len() && value != [0; 16] {
        return value.iter().map(|b| format!("{b:02x}")).collect();
    }
    info.path().to_string_lossy().into_owned()
}

/// Parse a UTF-16LE device property buffer into a String.
fn property_string(buffer: &[u8]) -> String {
    let units: Vec<u16> = buffer
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// Read one device property from a devnode addressed by instance id.
fn instance_property(instance: &[u16], key: &DEVPROPKEY, buffer: &mut [u8]) -> Option<usize> {
    with_device_set(|set| unsafe {
        let mut device = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..std::mem::zeroed()
        };
        if SetupDiOpenDeviceInfoW(set, instance.as_ptr(), std::ptr::null_mut(), 0, &mut device) == 0
        {
            return None;
        }
        node_property(set, &device, key, buffer)
    })
}

/// The physical USB node's location suffix (e.g. 6&3af0f9ce&0&8) tracks the
/// port. The immediate parent of a HID interface differs per firmware
/// personality - composite modes add a &MI_ function node and the instance
/// embeds the personality pid - so the chain is climbed to the node without
/// &MI_, whose location suffix survives re-enumeration and the 2104/2106
/// switch. The suffix is case-normalized: Windows varies it between runs.
pub(super) fn port_location(info: &DeviceInfo) -> String {
    let fallback = || info.path().to_string_lossy().into_owned();
    let mut buffer = [0u8; 1024];
    let mut node = match device_property(info, &DEVPKEY_Device_Parent, &mut buffer) {
        Some(read) if read >= 2 && read <= buffer.len() => property_string(&buffer[..read]),
        _ => return fallback(),
    };
    for _ in 0..3 {
        if !node.contains("&MI_") {
            break;
        }
        let wide: Vec<u16> = node.encode_utf16().chain(Some(0)).collect();
        match instance_property(&wide, &DEVPKEY_Device_Parent, &mut buffer) {
            Some(read) if read >= 2 && read <= buffer.len() => {
                node = property_string(&buffer[..read]);
            }
            _ => break,
        }
    }
    let location = node.rsplit('\\').next().unwrap_or_default().to_lowercase();
    if location.is_empty() {
        fallback()
    } else {
        location
    }
}
