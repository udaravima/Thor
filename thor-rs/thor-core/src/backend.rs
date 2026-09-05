//! Concrete USB transport backed by the pure-Rust `nusb` crate.
//!
//! This is the one platform-specific piece; everything else in the engine talks to the
//! [`Transport`](crate::transport::Transport) trait. nusb speaks usbfs directly on Linux
//! (no libusb — matching Thor's philosophy) and supports Windows/macOS too, so this same
//! backend is the path to cross-platform support.

use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, In, Out, TransferError};
use nusb::{list_devices, Device, Endpoint, Interface, MaybeFuture};

use crate::transport::{DeviceInfo, Transport, UsbError, SAMSUNG_VENDOR_ID};

/// USB class code for CDC-data interfaces — the one Samsung download mode presents.
const USB_CLASS_CDC_DATA: u8 = 0x0A;

fn backend_err<E: std::fmt::Display>(e: E) -> UsbError {
    UsbError::Backend(e.to_string())
}

fn map_transfer_err(e: TransferError) -> UsbError {
    match e {
        TransferError::Cancelled => UsbError::Timeout,
        other => UsbError::Backend(other.to_string()),
    }
}

/// Enumerate all connected Samsung devices (vendor `0x04E8`).
pub fn list_samsung_devices() -> Result<Vec<DeviceInfo>, UsbError> {
    let devices = list_devices().wait().map_err(backend_err)?;
    Ok(devices
        .filter(|d| d.vendor_id() == SAMSUNG_VENDOR_ID)
        .map(|d| DeviceInfo {
            display_name: d
                .product_string()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Samsung device".to_string()),
            identifier: format!("{}:{}", d.busnum(), d.device_address()),
            vendor_id: d.vendor_id(),
            product_id: d.product_id(),
        })
        .collect())
}

/// A live USB connection to a Samsung device in Odin download mode.
pub struct NusbTransport {
    _device: Device,
    _interface: Interface,
    ep_in: Endpoint<Bulk, In>,
    ep_out: Endpoint<Bulk, Out>,
}

impl NusbTransport {
    /// Open a specific device by its [`DeviceInfo::identifier`] (`busnum:address`), find its
    /// bulk IN/OUT endpoints, detach any kernel driver, and claim the interface.
    pub fn open(info: &DeviceInfo) -> Result<Self, UsbError> {
        let raw = list_devices()
            .wait()
            .map_err(backend_err)?
            .find(|d| {
                d.vendor_id() == SAMSUNG_VENDOR_ID
                    && format!("{}:{}", d.busnum(), d.device_address()) == info.identifier
            })
            .ok_or(UsbError::NoDevice)?;

        let device = raw
            .open()
            .wait()
            .map_err(|e| UsbError::Backend(format!("open device: {e}")))?;
        let (iface_num, in_addr, out_addr) = find_bulk_interface(&device)?;
        if std::env::var_os("THOR_DEBUG").is_some() {
            eprintln!(
                "[debug] interface {iface_num}, bulk IN {in_addr:#04x}, bulk OUT {out_addr:#04x}"
            );
        }

        // Detaches the kernel driver (our cdc_acm problem) and claims, in one call.
        let interface = device
            .detach_and_claim_interface(iface_num)
            .wait()
            .map_err(|e| UsbError::Backend(format!("claim interface {iface_num}: {e}")))?;
        let ep_in = interface
            .endpoint::<Bulk, In>(in_addr)
            .map_err(|e| UsbError::Backend(format!("open IN endpoint {in_addr:#04x}: {e}")))?;
        let ep_out = interface
            .endpoint::<Bulk, Out>(out_addr)
            .map_err(|e| UsbError::Backend(format!("open OUT endpoint {out_addr:#04x}: {e}")))?;

        Ok(NusbTransport { _device: device, _interface: interface, ep_in, ep_out })
    }
}

/// Find `(interface_number, bulk_in_addr, bulk_out_addr)`. Prefers a CDC-data interface
/// (like the C#), falling back to any interface that has both a bulk IN and OUT endpoint.
/// Direction is read from the endpoint address's high bit; nusb parses the descriptors.
fn find_bulk_interface(device: &Device) -> Result<(u8, u8, u8), UsbError> {
    let config = device.active_configuration().map_err(backend_err)?;
    let mut fallback: Option<(u8, u8, u8)> = None;
    for group in config.interfaces() {
        for alt in group.alt_settings() {
            let (mut in_addr, mut out_addr) = (None, None);
            for ep in alt.endpoints() {
                let addr = ep.address();
                if addr & 0x80 != 0 {
                    in_addr = Some(addr);
                } else {
                    out_addr = Some(addr);
                }
            }
            if let (Some(i), Some(o)) = (in_addr, out_addr) {
                let sel = (alt.interface_number(), i, o);
                if alt.class() == USB_CLASS_CDC_DATA {
                    return Ok(sel);
                }
                fallback.get_or_insert(sel);
            }
        }
    }
    fallback.ok_or(UsbError::NoEndpoints)
}

impl Transport for NusbTransport {
    fn bulk_write(&mut self, data: &[u8], timeout: Duration) -> Result<(), UsbError> {
        let completion = self.ep_out.transfer_blocking(Buffer::from(data), timeout);
        completion.status.map_err(map_transfer_err)?;
        Ok(())
    }

    fn bulk_read(&mut self, max_len: usize, timeout: Duration) -> Result<Vec<u8>, UsbError> {
        // nusb requires an IN buffer to be a nonzero multiple of the endpoint's max packet
        // size; the device ends the transfer early with a short packet, so we get back only
        // the bytes it actually sent (reported by `actual_len`).
        let mps = self.ep_in.max_packet_size().max(1);
        let requested = if max_len == 0 { mps } else { max_len.div_ceil(mps) * mps };
        let completion = self.ep_in.transfer_blocking(Buffer::new(requested), timeout);
        completion.status.map_err(map_transfer_err)?;
        Ok(completion.buffer[..completion.actual_len].to_vec())
    }
}
