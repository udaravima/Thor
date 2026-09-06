//! Samsung **Upload Mode** (S-Boot Upload Client, "SUC") — read RAM out of a device that is
//! in upload/ramdump mode over USB.
//!
//! This is a *separate protocol* from Odin/LOKE flashing, but it rides the exact same
//! [`Transport`](crate::transport::Transport): VID/PID `04e8:685d`, a class-10 bulk interface.
//! A device enters upload mode after a kernel panic or when armed via the `*#9900#` SysDump
//! menu. Dumping RAM is **read-only** — nothing is written to the device.
//!
//! Wire framing is ASCII, in Samsung's signature mixed-case magic-string style. Verified
//! against `bkerler/sboot_dump` (`samupload.py`); see `../../docs/port/experiments-kernel.md`.
//!
//! Why it matters: the kernel's `printk`/`dmesg` log is a ring buffer *in RAM*, so a RAM dump
//! is a superset of "get the kernel log over USB" — carve the log out of the dump offline.

use std::time::Duration;

use crate::transport::{Transport, UsbError};

/// Sent to probe/begin an exchange; a device in upload mode acknowledges it.
pub const PREAMBLE: &[u8] = b"PrEaMbLe\0";
/// Positive acknowledgement (both directions).
pub const ACK: &[u8] = b"AcKnOwLeDgMeNt\0";
/// Negative acknowledgement from the device.
pub const NAK: &[u8] = b"NeGaTiVeAcKmNt\0";
/// Request the region (partition) table.
pub const PROBE: &[u8] = b"PrObE\0";
/// Begin a data transfer of the previously-announced range.
pub const DATAXFER: &[u8] = b"DaTaXfEr\0";
/// End-of-transfer marker sent by the device.
pub const POSTAMBLE: &[u8] = b"PoStAmBlE\0";
/// Reboot the device out of upload mode.
pub const POWERDOWN: &[u8] = b"PoWeRdOwN\0";

/// The probe table starts after a 16-byte header.
const PROBE_HEADER_LEN: usize = 0x10;
/// Max bytes the device returns for a probe.
const PROBE_MAX: usize = 0x8000;
/// Default command/ack timeout.
const CMD_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout for reading a dump chunk (RAM reads can be slow).
const DUMP_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest single dump read we request (the backend caps to the endpoint's max packet size).
const DUMP_CHUNK: u64 = 1 << 20;

/// One dumpable region advertised by the device (a slice of RAM, or FTL/CP memory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Region type tag (device-defined).
    pub ptype: u32,
    /// Region name, e.g. `DRAM`, `TZ`, `CP` (ASCII, NUL-trimmed).
    pub name: String,
    /// Start address (inclusive).
    pub start: u64,
    /// End address (exclusive).
    pub end: u64,
}

impl Region {
    /// Size of the region in bytes (`end - start`, saturating).
    pub fn size(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

/// Anything that can go wrong speaking the upload protocol.
#[derive(Debug)]
pub enum UploadError {
    /// The underlying transport failed.
    Usb(UsbError),
    /// The device isn't in upload mode (no preamble ack).
    NotUploadMode,
    /// The device sent something unexpected.
    Protocol(String),
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadError::Usb(e) => write!(f, "{e}"),
            UploadError::NotUploadMode => write!(
                f,
                "device did not acknowledge the upload-mode preamble — it isn't in upload mode \
                 (arm it via a panic or the *#9900# SysDump menu)"
            ),
            UploadError::Protocol(m) => write!(f, "upload protocol error: {m}"),
        }
    }
}

impl std::error::Error for UploadError {}

impl From<UsbError> for UploadError {
    fn from(e: UsbError) -> Self {
        UploadError::Usb(e)
    }
}

/// Parse the raw probe response into the list of dumpable regions.
///
/// Layout (from `samupload.py`): a leading `+` byte means 64-bit addressing (entry size
/// `0x28`), otherwise 32-bit (entry size `0x1C`). Entries begin at offset `0x10`; each is
/// `ptype` (u32 LE), a fixed-length NUL-terminated ASCII `name`, then `start` and `end`
/// addresses (u64 or u32 LE). Parsing stops at end-of-buffer, at an all-zero entry, or at an
/// entry whose start address is below `20` (the sentinel `samupload.py` uses).
pub fn parse_probe_table(data: &[u8]) -> Vec<Region> {
    if data.len() < PROBE_HEADER_LEN {
        return Vec::new();
    }
    let is_64 = data[0] == b'+';
    let addr_size = if is_64 { 8 } else { 4 };
    let entry_size = if is_64 { 0x28 } else { 0x1C };
    // Name fills whatever is left between the u32 type and the two addresses (20 / 16 bytes).
    let name_len = entry_size - 4 - 2 * addr_size;
    let read_addr = |b: &[u8]| -> u64 {
        if is_64 {
            u64::from_le_bytes(b[..8].try_into().unwrap())
        } else {
            u32::from_le_bytes(b[..4].try_into().unwrap()) as u64
        }
    };

    let mut regions = Vec::new();
    let mut off = PROBE_HEADER_LEN;
    while off + entry_size <= data.len() {
        let e = &data[off..off + entry_size];
        let ptype = u32::from_le_bytes(e[0..4].try_into().unwrap());
        let name_bytes = &e[4..4 + name_len];
        let name_end = name_bytes.iter().position(|&c| c == 0).unwrap_or(name_len);
        let name = String::from_utf8_lossy(&name_bytes[..name_end]).into_owned();
        let start = read_addr(&e[4 + name_len..]);
        let end = read_addr(&e[4 + name_len + addr_size..]);
        // Stop at the all-zero terminator, or a bogus low start (samupload's sentinels).
        if (start == 0 && end == 0) || start < 20 {
            break;
        }
        regions.push(Region {
            ptype,
            name,
            start,
            end,
        });
        off += entry_size;
    }
    regions
}

/// An upload-mode session over a [`Transport`].
pub struct Upload<T: Transport> {
    transport: T,
}

impl<T: Transport> Upload<T> {
    /// Wrap a transport. No I/O happens until a command is issued.
    pub fn new(transport: T) -> Self {
        Upload { transport }
    }

    /// Recover the transport (e.g. to reboot/disconnect).
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Confirm the device is in upload mode: send [`PREAMBLE`], expect [`ACK`].
    pub fn handshake(&mut self) -> Result<(), UploadError> {
        self.transport.bulk_write(PREAMBLE, CMD_TIMEOUT)?;
        let resp = self.transport.bulk_read(64, CMD_TIMEOUT)?;
        // The device replies AcKnOwLeDgMeNt (upload mode) or NeGaTiVeAcKmNt / nothing.
        if resp.starts_with(b"AcKnOwLeDgMeNt") {
            Ok(())
        } else {
            Err(UploadError::NotUploadMode)
        }
    }

    /// Ask the device for its region table (`PrObE`), parsed via [`parse_probe_table`].
    pub fn probe(&mut self) -> Result<Vec<Region>, UploadError> {
        self.transport.bulk_write(PROBE, CMD_TIMEOUT)?;
        let data = self.transport.bulk_read(PROBE_MAX, CMD_TIMEOUT)?;
        Ok(parse_probe_table(&data))
    }

    /// Dump the half-open range `[start, end)`, handing each chunk to `sink` and reporting
    /// `(bytes_so_far, total)` to `progress`. Returns the number of bytes dumped.
    ///
    /// The address framing (fixed-width ASCII hex) is our best reading of `samupload.py` and
    /// is the one part that needs confirming against real hardware; the chunk/ack/postamble
    /// loop is exercised by the mock tests.
    pub fn dump_range(
        &mut self,
        start: u64,
        end: u64,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), UploadError>,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<u64, UploadError> {
        let total = end.saturating_sub(start);

        // Announce the range: preamble, start & end as fixed-width ASCII hex, then DATAXFER.
        self.transport.bulk_write(PREAMBLE, CMD_TIMEOUT)?;
        self.transport
            .bulk_write(format!("{start:016x}").as_bytes(), CMD_TIMEOUT)?;
        self.transport
            .bulk_write(format!("{end:016x}").as_bytes(), CMD_TIMEOUT)?;
        self.transport.bulk_write(DATAXFER, CMD_TIMEOUT)?;

        let mut pos: u64 = 0;
        while pos < total {
            let want = (total - pos).min(DUMP_CHUNK) as usize;
            let chunk = self.transport.bulk_read(want, DUMP_TIMEOUT)?;
            if chunk == POSTAMBLE {
                break; // device ended early
            }
            if chunk.is_empty() {
                return Err(UploadError::Protocol(
                    "device returned no data before the transfer completed".into(),
                ));
            }
            // Ack every chunk, then hand it to the sink.
            self.transport.bulk_write(ACK, CMD_TIMEOUT)?;
            sink(&chunk)?;
            pos += chunk.len() as u64;
            progress(pos, total);
        }

        // Consume the trailing postamble (best effort — ignore if the device didn't send one).
        let _ = self.transport.bulk_read(POSTAMBLE.len(), CMD_TIMEOUT);
        Ok(pos)
    }

    /// Reboot the device out of upload mode (`PoWeRdOwN`).
    pub fn power_down(&mut self) -> Result<(), UploadError> {
        self.transport.bulk_write(POWERDOWN, CMD_TIMEOUT)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct MockTransport {
        writes: Vec<Vec<u8>>,
        reads: VecDeque<Vec<u8>>,
    }
    impl MockTransport {
        fn with_reads(reads: Vec<Vec<u8>>) -> Self {
            MockTransport {
                writes: Vec::new(),
                reads: reads.into(),
            }
        }
    }
    impl Transport for MockTransport {
        fn bulk_write(&mut self, data: &[u8], _t: Duration) -> Result<(), UsbError> {
            self.writes.push(data.to_vec());
            Ok(())
        }
        fn bulk_read(&mut self, _max: usize, _t: Duration) -> Result<Vec<u8>, UsbError> {
            Ok(self.reads.pop_front().unwrap_or_default())
        }
    }

    /// Build a 64-bit probe buffer: 16-byte header (leading '+'), then 0x28-byte entries.
    fn probe64(entries: &[(u32, &str, u64, u64)]) -> Vec<u8> {
        let mut buf = vec![0u8; PROBE_HEADER_LEN];
        buf[0] = b'+';
        for &(ptype, name, start, end) in entries {
            let mut e = vec![0u8; 0x28];
            e[0..4].copy_from_slice(&ptype.to_le_bytes());
            let nb = name.as_bytes();
            e[4..4 + nb.len()].copy_from_slice(nb); // rest stays NUL
            e[0x18..0x20].copy_from_slice(&start.to_le_bytes());
            e[0x20..0x28].copy_from_slice(&end.to_le_bytes());
            buf.extend_from_slice(&e);
        }
        // all-zero terminator entry
        buf.extend_from_slice(&[0u8; 0x28]);
        buf
    }

    #[test]
    fn parse_probe_table_reads_64bit_entries() {
        let data = probe64(&[
            (1, "DRAM", 0x8000_0000, 0x8000_1000),
            (2, "TZ", 0x9000_0000, 0x9000_2000),
        ]);
        let regions = parse_probe_table(&data);
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].name, "DRAM");
        assert_eq!(regions[0].start, 0x8000_0000);
        assert_eq!(regions[0].end, 0x8000_1000);
        assert_eq!(regions[0].size(), 0x1000);
        assert_eq!(regions[1].name, "TZ");
        assert_eq!(regions[1].ptype, 2);
    }

    #[test]
    fn parse_probe_table_stops_at_zero_entry() {
        // A single real entry followed by the all-zero terminator → exactly one region.
        let data = probe64(&[(1, "DRAM", 0x8000_0000, 0x8000_1000)]);
        assert_eq!(parse_probe_table(&data).len(), 1);
    }

    #[test]
    fn parse_probe_table_32bit_entries() {
        // No leading '+' → 32-bit mode, entry size 0x1C, u32 addresses, name len 16.
        let mut buf = vec![0u8; PROBE_HEADER_LEN];
        buf[0] = b'X'; // not '+'
        let mut e = vec![0u8; 0x1C];
        e[0..4].copy_from_slice(&7u32.to_le_bytes());
        e[4..8].copy_from_slice(b"RAM\0");
        e[0x14..0x18].copy_from_slice(&0x4000_0000u32.to_le_bytes());
        e[0x18..0x1C].copy_from_slice(&0x4010_0000u32.to_le_bytes());
        buf.extend_from_slice(&e);
        buf.extend_from_slice(&[0u8; 0x1C]); // terminator

        let regions = parse_probe_table(&buf);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].name, "RAM");
        assert_eq!(regions[0].start, 0x4000_0000);
        assert_eq!(regions[0].end, 0x4010_0000);
    }

    #[test]
    fn parse_probe_table_empty_for_short_buffer() {
        assert!(parse_probe_table(&[0u8; 4]).is_empty());
    }

    #[test]
    fn handshake_sends_preamble_and_accepts_ack() {
        let mut mock = MockTransport::with_reads(vec![ACK.to_vec()]);
        {
            let mut up = Upload::new(&mut mock);
            up.handshake().expect("handshake ok");
        }
        assert_eq!(mock.writes[0], PREAMBLE);
    }

    #[test]
    fn handshake_rejects_non_upload_device() {
        let mut up = Upload::new(MockTransport::with_reads(vec![b"junk".to_vec()]));
        assert!(matches!(up.handshake(), Err(UploadError::NotUploadMode)));
    }

    #[test]
    fn dump_range_streams_chunks_and_stops_at_postamble() {
        // total = 8 bytes; device sends two 4-byte chunks then the postamble.
        let reads = vec![vec![0xAA; 4], vec![0xBB; 4], POSTAMBLE.to_vec()];
        let mut mock = MockTransport::with_reads(reads);
        let mut got = Vec::new();
        let dumped = {
            let mut up = Upload::new(&mut mock);
            up.dump_range(
                0x1000,
                0x1008,
                &mut |chunk| {
                    got.extend_from_slice(chunk);
                    Ok(())
                },
                &mut |_, _| {},
            )
            .expect("dump ok")
        };
        assert_eq!(dumped, 8);
        assert_eq!(got, [vec![0xAA; 4], vec![0xBB; 4]].concat());
        // Framing: PREAMBLE, start hex, end hex, DATAXFER were sent first…
        assert_eq!(mock.writes[0], PREAMBLE);
        assert_eq!(mock.writes[3], DATAXFER);
        // …then an ACK after each of the two data chunks.
        let acks = mock.writes.iter().filter(|w| w.as_slice() == ACK).count();
        assert_eq!(acks, 2);
    }

    #[test]
    fn probe_sends_probe_and_parses_regions() {
        let table = probe64(&[(1, "DRAM", 0x8000_0000, 0x8000_1000)]);
        let mut mock = MockTransport::with_reads(vec![table]);
        let regions = {
            let mut up = Upload::new(&mut mock);
            up.probe().expect("probe ok")
        };
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].name, "DRAM");
        assert_eq!(mock.writes[0], PROBE);
    }

    #[test]
    fn power_down_sends_powerdown() {
        let mut mock = MockTransport::with_reads(vec![]);
        {
            let mut up = Upload::new(&mut mock);
            up.power_down().expect("powerdown ok");
        }
        assert_eq!(mock.writes[0], POWERDOWN);
    }
}
