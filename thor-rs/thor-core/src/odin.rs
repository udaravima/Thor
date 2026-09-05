//! The Odin protocol: failure decoding now; the session state machine (handshake,
//! begin/dump/flash) is added once the USB transport trait exists.
//!
//! A device reply signals failure with `0xFF` in byte 0; the signed i32 at bytes 4..8 is
//! the error code. See `../../docs/odin-protocol.md`.

use std::time::Duration;

use crate::flash::FlashParams;
use crate::proto::{read_i32_le, Packet};
use crate::transport::{Transport, UsbError};

/// The reply's first byte when the bootloader reports failure.
const FAIL_MARKER: u8 = 0xFF;

/// Default timeout for ordinary Odin command/acknowledge exchanges.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Odin protocol region opcodes (byte 0 of a command packet).
mod region {
    pub const SESSION: i32 = 0x64;
    pub const PIT: i32 = 0x65;
    #[allow(dead_code)] // used from M2 onward (flashing)
    pub const FLASH: i32 = 0x66;
    #[allow(dead_code)]
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
            kind => write!(f, "device returned 0xFF (code 0x{:04X}, {:?})", self.code, kind),
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
    Err(OdinFailure { code, kind: FlashFailKind::from_code(code) })
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
}

impl<T: Transport> Odin<T> {
    /// Wrap a transport. No I/O happens until [`handshake`](Odin::handshake).
    pub fn new(transport: T) -> Self {
        Odin { transport, version: None, params: None }
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
}

/// Read exactly `want` bytes into an ack, or fail with a protocol error.
fn ack<T: Transport>(t: &mut T, want: usize) -> Result<Vec<u8>, OdinError> {
    let buf = t.bulk_read(want, DEFAULT_TIMEOUT)?;
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
            MockTransport { writes: Vec::new(), reads: reads.into() }
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
        let mut mock =
            MockTransport::with_reads(vec![size_reply, block0, block1, end_ack]);

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
        let err = OdinFailure { code: -5, kind: FlashFailKind::Auth };
        assert!(err.to_string().contains("Auth"));
    }
}
