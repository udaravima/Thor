//! The USB transport seam.
//!
//! Everything protocol-side depends only on the [`Transport`] trait, never on a specific
//! OS or USB library — mirroring the C# `IHandler` interface. The concrete Linux/Windows/
//! macOS backend (nusb) is one implementation; tests use a scripted mock. This is the seam
//! that keeps the port cross-platform.

use std::time::Duration;

/// Samsung's USB vendor id. Only devices with this vendor are considered.
pub const SAMSUNG_VENDOR_ID: u16 = 0x04E8;

/// A transport failure (no device, disconnected, timed out, or a backend error).
#[derive(Debug)]
pub enum UsbError {
    /// No matching device was found.
    NoDevice,
    /// A required endpoint/interface was not found on the device.
    NoEndpoints,
    /// The transport is not connected.
    NotConnected,
    /// The transfer timed out.
    Timeout,
    /// Any other backend error, with context.
    Backend(String),
}

impl std::fmt::Display for UsbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsbError::NoDevice => write!(f, "no matching Samsung device found"),
            UsbError::NoEndpoints => write!(f, "no valid CDC-data bulk endpoints found"),
            UsbError::NotConnected => write!(f, "not connected to a device"),
            UsbError::Timeout => write!(f, "USB transfer timed out"),
            UsbError::Backend(msg) => write!(f, "USB error: {msg}"),
        }
    }
}

impl std::error::Error for UsbError {}

/// A bidirectional bulk pipe to a device in Odin download mode.
///
/// The Odin protocol code is generic over this trait, so it can be driven by a real USB
/// backend or a scripted mock in tests.
pub trait Transport {
    /// Send `data` to the device's bulk OUT endpoint.
    fn bulk_write(&mut self, data: &[u8], timeout: Duration) -> Result<(), UsbError>;

    /// Read up to `max_len` bytes from the bulk IN endpoint, returning what the device
    /// actually sent (which may be fewer bytes).
    fn bulk_read(&mut self, max_len: usize, timeout: Duration) -> Result<Vec<u8>, UsbError>;

    /// Best-effort zero-length read, used to drain the IN endpoint after a PIT dump. The
    /// default reads zero bytes with a short timeout and ignores the result.
    fn read_zlp(&mut self) -> Result<(), UsbError> {
        let _ = self.bulk_read(0, Duration::from_millis(100));
        Ok(())
    }
}

/// Lets `&mut T` be used anywhere a `Transport` is expected (borrow a transport without
/// giving up ownership — handy for tests and for keeping a handle after a session).
impl<T: Transport + ?Sized> Transport for &mut T {
    fn bulk_write(&mut self, data: &[u8], timeout: Duration) -> Result<(), UsbError> {
        (**self).bulk_write(data, timeout)
    }
    fn bulk_read(&mut self, max_len: usize, timeout: Duration) -> Result<Vec<u8>, UsbError> {
        (**self).bulk_read(max_len, timeout)
    }
    fn read_zlp(&mut self) -> Result<(), UsbError> {
        (**self).read_zlp()
    }
}

/// A discovered Samsung device available to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Human-readable name (best effort).
    pub display_name: String,
    /// Stable identifier used to reopen this specific device (backend-defined).
    pub identifier: String,
    pub vendor_id: u16,
    pub product_id: u16,
}
