//! The Odin protocol: failure decoding now; the session state machine (handshake,
//! begin/dump/flash) is added once the USB transport trait exists.
//!
//! A device reply signals failure with `0xFF` in byte 0; the signed i32 at bytes 4..8 is
//! the error code. See `../../docs/odin-protocol.md`.

use crate::proto::read_i32_le;

/// The reply's first byte when the bootloader reports failure.
const FAIL_MARKER: u8 = 0xFF;

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

#[cfg(test)]
mod tests {
    use super::*;

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
