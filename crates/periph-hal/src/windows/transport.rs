//! HID I/O and owned Windows handles. No model matching or application state.
use super::wide;
use crate::{ErrorKind, HardwareError};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Devices::HumanInterfaceDevice::HidD_SetOutputReport,
    Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        ReadFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Threading::{CreateEventW, ResetEvent, WaitForSingleObject},
    },
};

pub(crate) fn open_pair(
    api: &HidApi,
    info: &DeviceInfo,
) -> Result<(HidDevice, HidDevice), HardwareError> {
    open_pair_path(api, info.path())
}

pub(crate) fn open_pair_path(
    api: &HidApi,
    path: &std::ffi::CStr,
) -> Result<(HidDevice, HidDevice), HardwareError> {
    // Keep input on an independent overlapped handle; never mix an outstanding
    // read with writes/IOCTL on the same handle.
    let reader = api
        .open_path(path)
        .map_err(HardwareError::transport)?;
    let writer = api
        .open_path(path)
        .map_err(HardwareError::transport)?;
    Ok((reader, writer))
}

pub fn send(writer: &HidDevice, data: &[u8]) -> Result<(), HardwareError> {
    writer
        .send_output_report(data)
        .or_else(|_| {
            writer.write(data).and_then(|n| {
                // windows-native returns 0 when WriteFile completes synchronously.
                if n == data.len() || n == 0 {
                    Ok(())
                } else {
                    Err(hidapi::HidError::HidApiError {
                        message: "incomplete output report".into(),
                    })
                }
            })
        })
        .map_err(HardwareError::transport)
}

pub fn drain(reader: &HidDevice) {
    let mut bytes = [0u8; 256];
    // Bounded even when a device continuously sends input.
    for _ in 0..32 {
        if !matches!(reader.read_timeout(&mut bytes, 0), Ok(n) if n > 0) {
            break;
        }
    }
}

/// KZZI keyboards behind a Compx 2.4G receiver. This interface only answers
/// requests written through the control endpoint (HidD_SetOutputReport) and
/// hidapi's buffer handling fails there (error 1784), so the exchange runs on
/// raw Windows handles: one handle for writes, one overlapped handle for reads.
pub struct KzziLink {
    writer: *mut std::ffi::c_void,
    reader: *mut std::ffi::c_void,
    event: *mut std::ffi::c_void,
}

impl KzziLink {
    pub fn open(path: &str) -> Result<Self, HardwareError> {
        let path = wide(path);
        let writer = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if writer == INVALID_HANDLE_VALUE {
            return Err(HardwareError::new(
                ErrorKind::Transport,
                "cannot open keyboard interface",
            ));
        }
        let reader = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if reader == INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(writer) };
            return Err(HardwareError::new(
                ErrorKind::Transport,
                "cannot open keyboard reader",
            ));
        }
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            unsafe {
                CloseHandle(writer);
                CloseHandle(reader);
            }
            return Err(HardwareError::new(
                ErrorKind::Transport,
                "cannot create keyboard read event",
            ));
        }
        Ok(Self {
            writer,
            reader,
            event,
        })
    }

    pub fn send(&self, frame: &[u8; 20]) -> Result<(), HardwareError> {
        let ok =
            unsafe { HidD_SetOutputReport(self.writer, frame.as_ptr().cast(), frame.len() as u32) };
        if ok {
            Ok(())
        } else {
            Err(HardwareError::new(
                ErrorKind::Transport,
                "keyboard query send failed",
            ))
        }
    }

    /// Cancel a pending overlapped read so the handle stays reusable.
    fn cancel(&self, pending: &mut OVERLAPPED) {
        unsafe {
            CancelIoEx(self.reader, pending as *mut _ as *mut _);
            WaitForSingleObject(self.event, 200);
            let mut read = 0u32;
            GetOverlappedResult(self.reader, pending, &mut read, 0);
        }
    }

    pub fn read(&self, buffer: &mut [u8], timeout: Duration) -> Option<usize> {
        let mut pending: OVERLAPPED = unsafe { std::mem::zeroed() };
        pending.hEvent = self.event;
        let mut read = 0u32;
        unsafe { ResetEvent(self.event) };
        let started = unsafe {
            ReadFile(
                self.reader,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
                &mut pending,
            )
        };
        if started != 0 {
            return (read > 0).then_some(read as usize);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.cancel(&mut pending);
                return None;
            }
            let wait =
                unsafe { WaitForSingleObject(self.event, remaining.as_millis().max(1) as u32) };
            if wait == WAIT_OBJECT_0 {
                let done = unsafe { GetOverlappedResult(self.reader, &pending, &mut read, 0) };
                return (done != 0 && read > 0).then_some(read as usize);
            }
        }
    }

    pub fn drain(&self) {
        let mut buffer = [0u8; 64];
        for _ in 0..32 {
            if self.read(&mut buffer, Duration::from_millis(8)).is_none() {
                break;
            }
        }
    }
}

impl Drop for KzziLink {
    fn drop(&mut self) {
        unsafe {
            CancelIoEx(self.reader, std::ptr::null_mut());
            CloseHandle(self.event);
            CloseHandle(self.reader);
            CloseHandle(self.writer);
        }
    }
}
