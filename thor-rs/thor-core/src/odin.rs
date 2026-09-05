//! The Odin protocol: failure decoding now; the session state machine (handshake,
//! begin/dump/flash) is added once the USB transport trait exists.
//!
//! A device reply signals failure with `0xFF` in byte 0; the signed i32 at bytes 4..8 is
//! the error code. See `../../docs/odin-protocol.md`.

use std::io::Read;
use std::time::Duration;

use crate::flash::{plan_flash, FlashParams};
use crate::pit::PitEntry;
use crate::proto::{read_i32_le, Packet};
use crate::transport::{Transport, UsbError};

/// The reply's first byte when the bootloader reports failure.
const FAIL_MARKER: u8 = 0xFF;

/// Default timeout for ordinary Odin command/acknowledge exchanges.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Long timeout for slow session commands — a userdata erase, T-Flash enable, or region set
/// can take minutes on the device. Matches the reference C#'s 600000 ms.
pub const LONG_TIMEOUT: Duration = Duration::from_secs(600);

/// Odin protocol region opcodes (byte 0 of a command packet).
mod region {
    pub const SESSION: i32 = 0x64;
    pub const PIT: i32 = 0x65;
    pub const FLASH: i32 = 0x66;
    pub const END: i32 = 0x67;
}

/// A decoded end-of-sequence flash failure reason. Other codes map to
/// [`FlashFailKind::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashFailKind {
    /// -2: partition is write-protected.
    WriteProtect,
    /// -3: erase step failed.
    Erase,
    /// -4: write step failed.
    Write,
    /// -5: signature/authentication rejected.
    Auth,
    /// -6: size mismatch.
    Size,
    /// -7: ext4-specific failure.
    Ext4,
    /// Any other code.
    Unknown,
}

impl FlashFailKind {
    fn from_code(code: i32) -> Self {
        match code {
            -2 => FlashFailKind::WriteProtect,
            -3 => FlashFailKind::Erase,
            -4 => FlashFailKind::Write,
            -5 => FlashFailKind::Auth,
            -6 => FlashFailKind::Size,
            -7 => FlashFailKind::Ext4,
            _ => FlashFailKind::Unknown,
        }
    }
}

/// A device reply that reported failure (`0xFF`), with its raw code and decoded kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OdinFailure {
    pub code: i32,
    pub kind: FlashFailKind,
}

impl std::fmt::Display for OdinFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            FlashFailKind::Unknown => write!(f, "device returned 0xFF (code 0x{:04X})", self.code),
            kind => write!(
                f,
                "device returned 0xFF (code 0x{:04X}, {:?})",
                self.code, kind
            ),
        }
    }
}

impl std::error::Error for OdinFailure {}

/// Check a device reply: `Ok(())` unless byte 0 is `0xFF`, in which case the decoded
/// failure is returned. Mirrors the C# `OdinFailCheck`, but always decodes the kind.
pub fn check_reply(buf: &[u8]) -> Result<(), OdinFailure> {
    if buf.first() != Some(&FAIL_MARKER) {
        return Ok(());
    }
    let code = read_i32_le(buf, 4).unwrap_or(0);
    Err(OdinFailure {
        code,
        kind: FlashFailKind::from_code(code),
    })
}

/// The bootloader version reported by [`Odin::begin_session`]. `unknown1`/`unknown2` are
/// almost always zero; non-zero values mark undiscovered bootloader capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub unknown1: u8,
    pub unknown2: u8,
    pub version: i16,
}

/// Anything that can go wrong while speaking Odin.
#[derive(Debug)]
pub enum OdinError {
    /// The underlying transport failed.
    Usb(UsbError),
    /// The device reported a failure (`0xFF`).
    Failure(OdinFailure),
    /// The device sent something unexpected (wrong handshake, short reply, etc.).
    Protocol(String),
}

impl std::fmt::Display for OdinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OdinError::Usb(e) => write!(f, "{e}"),
            OdinError::Failure(e) => write!(f, "{e}"),
            OdinError::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for OdinError {}

impl From<UsbError> for OdinError {
    fn from(e: UsbError) -> Self {
        OdinError::Usb(e)
    }
}

impl From<OdinFailure> for OdinError {
    fn from(e: OdinFailure) -> Self {
        OdinError::Failure(e)
    }
}

/// An Odin protocol session over a [`Transport`].
///
/// Holds the transport plus the bootloader version and flash parameters learned at
/// [`begin_session`](Odin::begin_session). Dropping it drops the transport, which ends the
/// session — the C#'s "disconnect leaves stale state" bug can't happen here.
pub struct Odin<T: Transport> {
    transport: T,
    version: Option<Version>,
    params: Option<FlashParams>,
    /// Set the EFS-clear bit in phone-firmware end-of-sequence packets.
    pub efs_clear: bool,
    /// Set the bootloader-update bit in phone-firmware end-of-sequence packets.
    pub bootloader_update: bool,
    /// Reset the flash counter after a successful flash (default true, matching the C#).
    pub reset_flash_count: bool,
    /// Whether T-Flash mode has been enabled this session (see [`enable_tflash`](Odin::enable_tflash)).
    tflash_enabled: bool,
}

impl<T: Transport> Odin<T> {
    /// Wrap a transport. No I/O happens until [`handshake`](Odin::handshake).
    pub fn new(transport: T) -> Self {
        Odin {
            transport,
            version: None,
            params: None,
            efs_clear: false,
            bootloader_update: false,
            reset_flash_count: true,
            tflash_enabled: false,
        }
    }

    /// Whether T-Flash mode has been enabled this session.
    pub fn tflash_enabled(&self) -> bool {
        self.tflash_enabled
    }

    /// The bootloader version, once [`begin_session`](Odin::begin_session) has run.
    pub fn version(&self) -> Option<Version> {
        self.version
    }

    /// The flash parameters selected for this session, once begun.
    pub fn params(&self) -> Option<FlashParams> {
        self.params
    }

    /// Recover the transport (e.g. to reboot/disconnect).
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Perform the ODIN → LOKE handshake.
    pub fn handshake(&mut self) -> Result<(), OdinError> {
        self.transport.bulk_write(b"ODIN", DEFAULT_TIMEOUT)?;
        let resp = self.transport.bulk_read(4, DEFAULT_TIMEOUT)?;
        if resp != b"LOKE" {
            return Err(OdinError::Protocol(format!(
                "expected LOKE handshake, got {resp:?}"
            )));
        }
        Ok(())
    }

    /// Begin a session: send `BeginSession`, learn the bootloader version, and (on new
    /// bootloaders) announce the flash packet size.
    pub fn begin_session(&mut self) -> Result<Version, OdinError> {
        // Proto version is deliberately maxed so the bootloader treats us as catch-all.
        let mut cmd = Packet::command(region::SESSION, 0x00);
        cmd.write_i32(8, i32::MAX);
        self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
        let resp = ack(&mut self.transport, 8)?;

        let version = Version {
            unknown1: resp[4],
            unknown2: resp[5],
            version: i16::from_le_bytes([resp[6], resp[7]]),
        };
        let params = FlashParams::for_bootloader_version(version.version);

        // New bootloaders expect us to announce the flash packet size up front.
        if version.version > 1 {
            let mut cmd = Packet::command(region::SESSION, 0x05);
            cmd.write_i32(8, params.packet_size as i32);
            self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
            ack(&mut self.transport, 8)?;
        }

        self.version = Some(version);
        self.params = Some(params);
        Ok(version)
    }

    /// Dump the device's PIT (partition table) as raw bytes.
    pub fn dump_pit(&mut self) -> Result<Vec<u8>, OdinError> {
        // 1. Request the dump; the reply carries the total size at bytes 4..8.
        let cmd = Packet::command(region::PIT, 0x01);
        self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
        let resp = ack(&mut self.transport, 8)?;
        let size = read_i32_le(&resp, 4)
            .filter(|&s| s >= 0)
            .ok_or_else(|| OdinError::Protocol("invalid PIT size".into()))?
            as usize;

        // 2. Read the PIT in 500-byte blocks.
        let mut pit = vec![0u8; size];
        let blocks = size.div_ceil(500);
        for i in 0..blocks {
            let mut cmd = Packet::command(region::PIT, 0x02);
            cmd.write_i32(8, i as i32);
            self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
            let block = self.transport.bulk_read(500, DEFAULT_TIMEOUT)?;
            let offset = i * 500;
            // Copy only what fits — the final block is partial. (The C# could overrun here.)
            let n = block.len().min(size - offset);
            pit[offset..offset + n].copy_from_slice(&block[..n]);
        }

        // 3. Drain any trailing zero-length packet (best effort), then end the dump.
        self.transport.read_zlp()?;
        let cmd = Packet::command(region::PIT, 0x03);
        self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
        ack(&mut self.transport, 8)?;

        Ok(pit)
    }

    /// End the Odin session cleanly (`EndSession`, 0x67/0x00), leaving the device in
    /// download mode.
    pub fn end_session(&mut self) -> Result<(), OdinError> {
        self.simple_command(region::END, 0x00)
    }

    /// Reboot into normal Android (0x67/0x01).
    pub fn reboot(&mut self) -> Result<(), OdinError> {
        self.simple_command(region::END, 0x01)
    }

    /// Reboot back into download mode (0x67/0x02). Not supported on every device.
    pub fn reboot_to_odin(&mut self) -> Result<(), OdinError> {
        self.simple_command(region::END, 0x02)
    }

    /// Power the device off (0x67/0x03).
    pub fn shutdown(&mut self) -> Result<(), OdinError> {
        self.simple_command(region::END, 0x03)
    }

    /// A command with no arguments that just expects an 8-byte ack.
    fn simple_command(&mut self, region: i32, sub: i32) -> Result<(), OdinError> {
        let cmd = Packet::command(region, sub);
        self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
        ack(&mut self.transport, 8)?;
        Ok(())
    }

    /// Announce the total number of bytes about to be flashed (`SetTotalBytes`, 0x64/0x02).
    /// Call once before flashing partitions.
    pub fn set_total_bytes(&mut self, total: i64) -> Result<(), OdinError> {
        let mut cmd = Packet::command(region::SESSION, 0x02);
        cmd.write_i64(8, total);
        self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
        ack(&mut self.transport, 8)?;
        Ok(())
    }

    /// Erase the userdata partition — a **factory reset** (`0x64/0x07`). Destructive and slow
    /// (can take minutes), so it uses [`LONG_TIMEOUT`].
    ///
    /// Note: on a device with an unlocked bootloader this can trip Samsung's *VaultKeeper*,
    /// which re-locks the bootloader after `/data` is wiped until setup is completed online.
    pub fn erase_user_data(&mut self) -> Result<(), OdinError> {
        let cmd = Packet::command(region::SESSION, 0x07);
        self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
        ack_with(&mut self.transport, 8, LONG_TIMEOUT)?;
        Ok(())
    }

    /// Enable **T-Flash** mode (`0x64/0x08`): the *next* flash targets an inserted microSD
    /// card instead of internal storage. Errors if already enabled. Uses [`LONG_TIMEOUT`].
    pub fn enable_tflash(&mut self) -> Result<(), OdinError> {
        if self.tflash_enabled {
            return Err(OdinError::Protocol(
                "T-Flash mode is already enabled".into(),
            ));
        }
        let cmd = Packet::command(region::SESSION, 0x08);
        self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
        ack_with(&mut self.transport, 8, LONG_TIMEOUT)?;
        self.tflash_enabled = true;
        Ok(())
    }

    /// Set the device **region (CSC) code** — exactly 3 characters (`0x64/0x08` + string).
    ///
    /// UNVERIFIED: in the reference C# this shares sub-command `0x08` with
    /// [`enable_tflash`](Odin::enable_tflash) — almost certainly a copy-paste bug — so on a
    /// real device this may actually just enable T-Flash rather than change the region. Ported
    /// faithfully and gated; do not rely on it until confirmed on hardware. See roadmap F8.
    pub fn set_region_code(&mut self, code: &str) -> Result<(), OdinError> {
        if code.len() != 3 {
            return Err(OdinError::Protocol(format!(
                "region code must be exactly 3 characters, got {}",
                code.len()
            )));
        }
        let mut cmd = Packet::command(region::SESSION, 0x08);
        cmd.write_str(8, code);
        self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
        ack_with(&mut self.transport, 8, LONG_TIMEOUT)?;
        Ok(())
    }

    /// Flash one partition (region 0x66).
    ///
    /// `source` provides the image bytes; `None` writes zeros (used for erase). `length` is
    /// the number of bytes to flash (the caller knows it — a `Read` has no length). `entry`
    /// supplies the partition's ids/type for the end-of-sequence packet, and `progress` is
    /// called as bytes are sent. **Destructive** against a real device.
    pub fn flash_partition(
        &mut self,
        mut source: Option<&mut dyn Read>,
        entry: &PitEntry,
        length: i64,
        mut progress: impl FnMut(FlashProgress),
    ) -> Result<(), OdinError> {
        let params = self
            .params
            .ok_or_else(|| OdinError::Protocol("no active session — call begin_session".into()))?;
        let packet_size = params.packet_size;
        let flash_timeout = Duration::from_millis(params.flash_timeout_ms);

        // Request file flash (0x66/0x00).
        let cmd = Packet::command(region::FLASH, 0x00);
        self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
        ack(&mut self.transport, 8)?;

        let plan = plan_flash(length, &params);
        let total_sequences = plan.len();
        let mut sent_bytes: i64 = 0;

        for seq in &plan {
            progress(FlashProgress {
                sequence_index: seq.index,
                total_sequences,
                sent_bytes,
                total_bytes: length,
                state: FlashState::Sending,
            });

            // Request sequence flash (0x66/0x02) with the packet-aligned size.
            let mut cmd = Packet::command(region::FLASH, 0x02);
            cmd.write_i32(8, seq.aligned_size as i32);
            self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
            ack(&mut self.transport, 8)?;

            // Send the sequence as `parts` packets of `packet_size` bytes each. A short read
            // (or `None` source, for erase) leaves the rest of the packet zero-padded.
            for j in 0..seq.parts {
                let mut part = vec![0u8; packet_size as usize];
                if let Some(src) = source.as_mut() {
                    read_fill(&mut **src, &mut part)?;
                }
                self.transport.bulk_write(&part, DEFAULT_TIMEOUT)?;
                let reply = ack(&mut self.transport, 8)?;
                let index = read_i32_le(&reply, 4).unwrap_or(-1) as i64;
                if index != j {
                    return Err(OdinError::Protocol(format!(
                        "expected part index {j}, device acknowledged {index}"
                    )));
                }
                sent_bytes += packet_size;
                progress(FlashProgress {
                    sequence_index: seq.index,
                    total_sequences,
                    sent_bytes,
                    total_bytes: length,
                    state: FlashState::Sending,
                });
            }

            progress(FlashProgress {
                sequence_index: seq.index,
                total_sequences,
                sent_bytes,
                total_bytes: length,
                state: FlashState::Flashing,
            });

            // End sequence flash (0x66/0x03). Modem firmware uses a shorter layout without a
            // partition id or the EFS/bootloader flags.
            let mut cmd = Packet::command(region::FLASH, 0x03);
            if entry.binary_type == 1 {
                cmd.write_i32(8, 0x01);
                cmd.write_i32(12, seq.real_size as i32);
                cmd.write_i32(16, entry.binary_type);
                cmd.write_i32(20, entry.device_type);
                cmd.write_i32(24, if seq.is_last { 1 } else { 0 });
            } else {
                cmd.write_i32(8, 0x00);
                cmd.write_i32(12, seq.real_size as i32);
                cmd.write_i32(16, entry.binary_type);
                cmd.write_i32(20, entry.device_type);
                cmd.write_i32(24, entry.partition_id);
                cmd.write_i32(28, if seq.is_last { 1 } else { 0 });
                cmd.write_i32(32, i32::from(self.efs_clear));
                cmd.write_i32(36, i32::from(self.bootloader_update));
            }
            self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
            // The device commits the sequence here — allow the version-based flash timeout.
            let reply = self.transport.bulk_read(8, flash_timeout)?;
            if reply.len() != 8 {
                return Err(OdinError::Protocol(format!(
                    "expected 8-byte end-sequence reply, got {}",
                    reply.len()
                )));
            }
            check_reply(&reply)?;
        }

        // Reset the flash counter (0x64/0x01), unless disabled.
        if self.reset_flash_count {
            let cmd = Packet::command(region::SESSION, 0x01);
            self.transport.bulk_write(cmd.as_bytes(), DEFAULT_TIMEOUT)?;
            ack(&mut self.transport, 8)?;
        }

        Ok(())
    }
}

/// Fill `buf` from `reader`, reading until it's full or EOF; bytes past EOF stay as they
/// were (the caller pre-zeros for padding). Handles partial reads.
fn read_fill(reader: &mut dyn Read, buf: &mut [u8]) -> Result<(), OdinError> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(OdinError::Protocol(format!("reading flash source: {e}"))),
        }
    }
    Ok(())
}

/// Whether a flash sequence is currently being sent to the device or committed by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashState {
    Sending,
    Flashing,
}

/// Progress of a partition flash, reported to the `flash_partition` callback.
#[derive(Debug, Clone, Copy)]
pub struct FlashProgress {
    pub sequence_index: usize,
    pub total_sequences: usize,
    pub sent_bytes: i64,
    pub total_bytes: i64,
    pub state: FlashState,
}

/// Read exactly `want` bytes into an ack (using [`DEFAULT_TIMEOUT`]), or fail with a protocol
/// error.
fn ack<T: Transport>(t: &mut T, want: usize) -> Result<Vec<u8>, OdinError> {
    ack_with(t, want, DEFAULT_TIMEOUT)
}

/// Like [`ack`], but with an explicit timeout — for slow commands (erase, T-Flash, region).
fn ack_with<T: Transport>(t: &mut T, want: usize, timeout: Duration) -> Result<Vec<u8>, OdinError> {
    let buf = t.bulk_read(want, timeout)?;
    if buf.len() != want {
        return Err(OdinError::Protocol(format!(
            "expected {want}-byte reply, got {}",
            buf.len()
        )));
    }
    check_reply(&buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Transport;
    use std::collections::VecDeque;

    /// A scripted transport: records every write, replays queued reads in order.
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
        // Keep tests focused: don't consume a scripted read for the drain.
        fn read_zlp(&mut self) -> Result<(), UsbError> {
            Ok(())
        }
    }

    /// A little-endian i16 version encoded as the bootloader sends it in bytes 6..8 of the
    /// BeginSession reply (bytes 0..4 status, byte 4/5 unknown, byte 6/7 version).
    fn begin_reply(version: i16) -> Vec<u8> {
        let mut r = vec![0u8; 8];
        r[6..8].copy_from_slice(&version.to_le_bytes());
        r
    }

    #[test]
    fn handshake_sends_odin_and_accepts_loke() {
        let mut odin = Odin::new(MockTransport::with_reads(vec![b"LOKE".to_vec()]));
        odin.handshake().expect("handshake ok");
        odin.into_transport(); // move out to inspect below via a fresh run
    }

    #[test]
    fn handshake_writes_the_literal_odin_bytes() {
        let mut mock = MockTransport::with_reads(vec![b"LOKE".to_vec()]);
        // drive via a borrowed Odin so we can read `writes` afterwards
        {
            let mut odin = Odin::new(&mut mock);
            odin.handshake().expect("handshake ok");
        }
        assert_eq!(mock.writes.len(), 1);
        assert_eq!(mock.writes[0], b"ODIN");
    }

    #[test]
    fn handshake_rejects_wrong_response() {
        let mut odin = Odin::new(MockTransport::with_reads(vec![b"FAIL".to_vec()]));
        assert!(matches!(odin.handshake(), Err(OdinError::Protocol(_))));
    }

    #[test]
    fn begin_session_new_bootloader_parses_version_and_sends_packet_size() {
        // reply 1: version 2; reply 2: ack for the packet-size command
        let mut mock = MockTransport::with_reads(vec![begin_reply(2), vec![0u8; 8]]);
        let version = {
            let mut odin = Odin::new(&mut mock);
            odin.begin_session().expect("begin ok")
        };
        assert_eq!(version.version, 2);
        // two commands were sent: BeginSession, then SendFilePartSize
        assert_eq!(mock.writes.len(), 2);
        // BeginSession: region 0x64, sub 0x00
        assert_eq!(&mock.writes[0][0..4], &0x64i32.to_le_bytes());
        assert_eq!(&mock.writes[0][4..8], &0x00i32.to_le_bytes());
        // SendFilePartSize: region 0x64, sub 0x05, 1 MiB at offset 8
        assert_eq!(&mock.writes[1][4..8], &0x05i32.to_le_bytes());
        assert_eq!(&mock.writes[1][8..12], &1_048_576i32.to_le_bytes());
    }

    #[test]
    fn begin_session_old_bootloader_skips_packet_size() {
        let mut mock = MockTransport::with_reads(vec![begin_reply(1)]);
        {
            let mut odin = Odin::new(&mut mock);
            let v = odin.begin_session().expect("begin ok");
            assert_eq!(v.version, 1);
            assert_eq!(odin.params().unwrap().packet_size, 131_072);
        }
        assert_eq!(mock.writes.len(), 1); // only BeginSession, no packet-size command
    }

    #[test]
    fn begin_session_propagates_device_failure() {
        let mut fail = vec![0xFFu8, 0, 0, 0];
        fail.extend_from_slice(&(-5i32).to_le_bytes());
        let mut odin = Odin::new(MockTransport::with_reads(vec![fail]));
        match odin.begin_session() {
            Err(OdinError::Failure(f)) => assert_eq!(f.kind, FlashFailKind::Auth),
            other => panic!("expected auth failure, got {other:?}"),
        }
    }

    #[test]
    fn dump_pit_assembles_blocks_and_truncates_to_size() {
        // request reply: size = 700 at bytes 4..8
        let mut size_reply = vec![0u8; 8];
        size_reply[4..8].copy_from_slice(&700i32.to_le_bytes());
        let block0 = vec![0xAAu8; 500];
        let block1 = vec![0xBBu8; 200]; // partial final block
        let end_ack = vec![0u8; 8];
        let mut mock = MockTransport::with_reads(vec![size_reply, block0, block1, end_ack]);

        let pit = {
            let mut odin = Odin::new(&mut mock);
            odin.dump_pit().expect("dump ok")
        };

        assert_eq!(pit.len(), 700);
        assert!(pit[..500].iter().all(|&b| b == 0xAA));
        assert!(pit[500..700].iter().all(|&b| b == 0xBB));
        // block-read commands carried region 0x65 sub 0x02 with indices 0 and 1
        // writes: [request 0x65/0x01, block 0x65/0x02 idx0, block idx1, end 0x65/0x03]
        assert_eq!(&mock.writes[1][8..12], &0i32.to_le_bytes());
        assert_eq!(&mock.writes[2][8..12], &1i32.to_le_bytes());
    }

    use crate::flash::FlashParams;
    use crate::pit::PitEntry;
    use std::io::Cursor;

    fn ack0() -> Vec<u8> {
        vec![0u8; 8]
    }
    fn ack_index(i: i32) -> Vec<u8> {
        let mut a = vec![0u8; 8];
        a[4..8].copy_from_slice(&i.to_le_bytes());
        a
    }
    /// Tiny params so a flash produces a handful of small parts we can assert on.
    fn tiny_params() -> FlashParams {
        FlashParams {
            packet_size: 4,
            packets_per_sequence: 2,
            flash_timeout_ms: 1000,
        }
    }
    fn le(v: i32) -> [u8; 4] {
        v.to_le_bytes()
    }

    #[test]
    fn session_lifecycle_commands_send_0x67_opcodes() {
        let mut mock = MockTransport::with_reads(vec![ack0(), ack0(), ack0(), ack0()]);
        {
            let mut odin = Odin::new(&mut mock);
            odin.end_session().unwrap();
            odin.reboot().unwrap();
            odin.reboot_to_odin().unwrap();
            odin.shutdown().unwrap();
        }
        let expect = [(0x67, 0x00), (0x67, 0x01), (0x67, 0x02), (0x67, 0x03)];
        for (i, (region, sub)) in expect.iter().enumerate() {
            assert_eq!(&mock.writes[i][0..4], le(*region).as_slice());
            assert_eq!(&mock.writes[i][4..8], le(*sub).as_slice());
        }
    }

    #[test]
    fn erase_user_data_sends_session_0x07() {
        let mut mock = MockTransport::with_reads(vec![ack0()]);
        {
            let mut odin = Odin::new(&mut mock);
            odin.erase_user_data().unwrap();
        }
        assert_eq!(&mock.writes[0][0..4], &le(0x64));
        assert_eq!(&mock.writes[0][4..8], &le(0x07));
    }

    #[test]
    fn enable_tflash_sends_0x08_and_sets_flag() {
        let mut mock = MockTransport::with_reads(vec![ack0()]);
        let enabled = {
            let mut odin = Odin::new(&mut mock);
            odin.enable_tflash().unwrap();
            odin.tflash_enabled()
        };
        assert!(enabled, "T-Flash flag should be set after enable");
        assert_eq!(&mock.writes[0][0..4], &le(0x64));
        assert_eq!(&mock.writes[0][4..8], &le(0x08));
    }

    #[test]
    fn enable_tflash_twice_errors() {
        let mut mock = MockTransport::with_reads(vec![ack0(), ack0()]);
        let mut odin = Odin::new(&mut mock);
        odin.enable_tflash().unwrap();
        assert!(matches!(odin.enable_tflash(), Err(OdinError::Protocol(_))));
    }

    #[test]
    fn set_region_code_writes_three_char_code() {
        let mut mock = MockTransport::with_reads(vec![ack0()]);
        {
            let mut odin = Odin::new(&mut mock);
            odin.set_region_code("XAA").unwrap();
        }
        assert_eq!(&mock.writes[0][0..4], &le(0x64));
        assert_eq!(&mock.writes[0][4..8], &le(0x08));
        assert_eq!(&mock.writes[0][8..11], b"XAA");
    }

    #[test]
    fn set_region_code_rejects_wrong_length() {
        let mut mock = MockTransport::with_reads(vec![]);
        let mut odin = Odin::new(&mut mock);
        assert!(matches!(
            odin.set_region_code("XA"),
            Err(OdinError::Protocol(_))
        ));
    }

    #[test]
    fn set_total_bytes_sends_long_command() {
        let mut mock = MockTransport::with_reads(vec![ack0()]);
        {
            let mut odin = Odin::new(&mut mock);
            odin.set_total_bytes(0x0102_0304_0506_0708).unwrap();
        }
        assert_eq!(&mock.writes[0][0..4], &le(0x64)); // session region
        assert_eq!(&mock.writes[0][4..8], &le(0x02)); // SetTotalBytes
        assert_eq!(
            &mock.writes[0][8..16],
            &0x0102_0304_0506_0708i64.to_le_bytes()
        );
    }

    #[test]
    fn flash_partition_phone_sends_full_sequence() {
        // 10 bytes with packet_size 4, 2 packets/seq (seq = 8 bytes):
        //   seq0: real 8, aligned 8, 2 parts;  seq1: real 2, aligned 4, 1 part (zero-padded)
        let reads = vec![
            ack0(),       // request file flash
            ack0(),       // seq0 request sequence
            ack_index(0), // seq0 part 0
            ack_index(1), // seq0 part 1
            ack0(),       // seq0 end sequence
            ack0(),       // seq1 request sequence
            ack_index(0), // seq1 part 0
            ack0(),       // seq1 end sequence
            ack0(),       // reset flash count
        ];
        let mut mock = MockTransport::with_reads(reads);
        let entry = PitEntry {
            binary_type: 0,
            device_type: 2,
            partition_id: 5,
            ..Default::default()
        };
        let data: Vec<u8> = (0..10).collect();
        {
            let mut odin = Odin::new(&mut mock);
            odin.params = Some(tiny_params());
            let mut cursor = Cursor::new(data);
            odin.flash_partition(Some(&mut cursor), &entry, 10, |_| {})
                .unwrap();
        }
        let w = &mock.writes;
        // request file flash
        assert_eq!(&w[0][0..4], &le(0x66));
        assert_eq!(&w[0][4..8], &le(0x00));
        // seq0 request sequence, aligned size 8
        assert_eq!(&w[1][4..8], &le(0x02));
        assert_eq!(&w[1][8..12], &le(8));
        // part data (raw packet_size buffers)
        assert_eq!(w[2], vec![0, 1, 2, 3]);
        assert_eq!(w[3], vec![4, 5, 6, 7]);
        // seq0 end sequence — phone layout
        assert_eq!(&w[4][4..8], &le(0x03));
        assert_eq!(&w[4][8..12], &le(0x00)); // 0 = phone
        assert_eq!(&w[4][12..16], &le(8)); // real size
        assert_eq!(&w[4][16..20], &le(0)); // binary type
        assert_eq!(&w[4][20..24], &le(2)); // device type
        assert_eq!(&w[4][24..28], &le(5)); // partition id
        assert_eq!(&w[4][28..32], &le(0)); // not last
                                           // seq1 request sequence, aligned size 4
        assert_eq!(&w[5][8..12], &le(4));
        // seq1 part 0 — 2 real bytes, zero-padded to 4
        assert_eq!(w[6], vec![8, 9, 0, 0]);
        // seq1 end sequence — real size 2, last = 1
        assert_eq!(&w[7][12..16], &le(2));
        assert_eq!(&w[7][28..32], &le(1));
        // reset flash count
        assert_eq!(&w[8][0..4], &le(0x64));
        assert_eq!(&w[8][4..8], &le(0x01));
    }

    #[test]
    fn flash_partition_modem_uses_modem_layout() {
        // length 4 → single sequence, single part
        let reads = vec![ack0(), ack0(), ack_index(0), ack0(), ack0()];
        let mut mock = MockTransport::with_reads(reads);
        let entry = PitEntry {
            binary_type: 1,
            device_type: 2,
            partition_id: 9,
            ..Default::default()
        };
        {
            let mut odin = Odin::new(&mut mock);
            odin.params = Some(tiny_params());
            let mut cursor = Cursor::new(vec![1u8, 2, 3, 4]);
            odin.flash_partition(Some(&mut cursor), &entry, 4, |_| {})
                .unwrap();
        }
        // end sequence is writes[3]
        let end = &mock.writes[3];
        assert_eq!(&end[4..8], &le(0x03));
        assert_eq!(&end[8..12], &le(0x01)); // 1 = modem
        assert_eq!(&end[12..16], &le(4)); // real size
        assert_eq!(&end[16..20], &le(1)); // binary type
        assert_eq!(&end[20..24], &le(2)); // device type
        assert_eq!(&end[24..28], &le(1)); // last (modem: no partition id, last is at offset 24)
    }

    #[test]
    fn flash_partition_erase_writes_zeros() {
        let reads = vec![ack0(), ack0(), ack_index(0), ack0(), ack0()];
        let mut mock = MockTransport::with_reads(reads);
        let entry = PitEntry {
            binary_type: 0,
            device_type: 2,
            partition_id: 3,
            ..Default::default()
        };
        {
            let mut odin = Odin::new(&mut mock);
            odin.params = Some(tiny_params());
            odin.flash_partition(None, &entry, 4, |_| {}).unwrap(); // None source = erase
        }
        assert_eq!(mock.writes[2], vec![0, 0, 0, 0]); // the part is all zeros
    }

    #[test]
    fn flash_partition_aborts_on_wrong_part_index() {
        // part 0 ack claims index 5 → mismatch → abort
        let reads = vec![ack0(), ack0(), ack_index(5)];
        let mut mock = MockTransport::with_reads(reads);
        let entry = PitEntry {
            binary_type: 0,
            ..Default::default()
        };
        let mut odin = Odin::new(&mut mock);
        odin.params = Some(tiny_params());
        let mut cursor = Cursor::new(vec![1u8, 2, 3, 4]);
        let r = odin.flash_partition(Some(&mut cursor), &entry, 4, |_| {});
        assert!(matches!(r, Err(OdinError::Protocol(_))));
    }

    #[test]
    fn success_reply_is_ok() {
        // A normal ack: first byte is not 0xFF.
        assert!(check_reply(&[0, 0, 0, 0, 0, 0, 0, 0]).is_ok());
    }

    #[test]
    fn auth_failure_is_decoded() {
        // 0xFF marker, code -5 (Auth) little-endian at bytes 4..8.
        let mut reply = vec![0xFF, 0, 0, 0];
        reply.extend_from_slice(&(-5i32).to_le_bytes());
        let err = check_reply(&reply).unwrap_err();
        assert_eq!(err.code, -5);
        assert_eq!(err.kind, FlashFailKind::Auth);
    }

    #[test]
    fn write_protect_and_ext4_codes_map() {
        let wp = {
            let mut r = vec![0xFF, 0, 0, 0];
            r.extend_from_slice(&(-2i32).to_le_bytes());
            check_reply(&r).unwrap_err()
        };
        assert_eq!(wp.kind, FlashFailKind::WriteProtect);

        let ext4 = {
            let mut r = vec![0xFF, 0, 0, 0];
            r.extend_from_slice(&(-7i32).to_le_bytes());
            check_reply(&r).unwrap_err()
        };
        assert_eq!(ext4.kind, FlashFailKind::Ext4);
    }

    #[test]
    fn unknown_code_keeps_raw_value() {
        let mut reply = vec![0xFF, 0, 0, 0];
        reply.extend_from_slice(&(-100i32).to_le_bytes());
        let err = check_reply(&reply).unwrap_err();
        assert_eq!(err.code, -100);
        assert_eq!(err.kind, FlashFailKind::Unknown);
    }

    #[test]
    fn display_names_the_kind() {
        let err = OdinFailure {
            code: -5,
            kind: FlashFailKind::Auth,
        };
        assert!(err.to_string().contains("Auth"));
    }
}
