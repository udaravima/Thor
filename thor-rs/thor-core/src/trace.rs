//! Wire tracing — a debug view of the raw USB traffic, so contributors can *see how the
//! device is talking*.
//!
//! Tracing is a process-wide toggle (a debug convenience, not shared domain state): flip it
//! with [`set_trace`], and the [`backend`](crate::backend) logs every bulk write/read to
//! stderr — each outgoing command decoded to its Odin region/sub-command name, with a short
//! hex preview. Enable it with `THOR_DEBUG=1`, the CLI's `--debug` flag, or `debug on` in the
//! shell.

use std::sync::atomic::{AtomicBool, Ordering};

static TRACE: AtomicBool = AtomicBool::new(false);

/// Turn wire tracing on or off.
pub fn set_trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

/// Whether wire tracing is currently on.
pub fn trace_enabled() -> bool {
    TRACE.load(Ordering::Relaxed)
}

/// Log an outgoing write (a no-op unless tracing is on).
pub fn log_write(data: &[u8]) {
    if trace_enabled() {
        eprintln!("→ {}   {}", describe_command(data), hex_preview(data, 16));
    }
}

/// Log an incoming read (a no-op unless tracing is on).
pub fn log_read(data: &[u8]) {
    if trace_enabled() {
        eprintln!("← {}B   {}", data.len(), hex_preview(data, 16));
    }
}

/// A compact hex preview of up to `max` bytes, with `…` if there are more.
pub fn hex_preview(data: &[u8], max: usize) -> String {
    let n = data.len().min(max);
    let mut s = data[..n]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    if data.len() > max {
        s.push_str(" …");
    }
    s
}

/// Decode an outgoing buffer into a human label: the ODIN handshake, or an Odin command's
/// region/sub-command with its known name. Falls back to just the length.
pub fn describe_command(data: &[u8]) -> String {
    if data == b"ODIN" {
        return "ODIN (handshake) [4B]".to_string();
    }
    if data.len() < 8 {
        return format!("[{}B]", data.len());
    }
    let region = i32::from_le_bytes(data[0..4].try_into().unwrap());
    let sub = i32::from_le_bytes(data[4..8].try_into().unwrap());
    format!(
        "{} (region=0x{region:02X} sub=0x{sub:02X}) [{}B]",
        command_name(region, sub),
        data.len()
    )
}

/// The known name for an Odin `(region, sub_command)` pair, or `"?"`.
fn command_name(region: i32, sub: i32) -> &'static str {
    match (region, sub) {
        (0x64, 0x00) => "BeginSession",
        (0x64, 0x01) => "ResetFlashCount",
        (0x64, 0x02) => "SetTotalBytes",
        (0x64, 0x05) => "SendFilePartSize",
        (0x64, 0x07) => "EraseUserData",
        (0x64, 0x08) => "EnableTFlash/SetRegion",
        (0x65, 0x00) => "FlashPitRequest",
        (0x65, 0x01) => "DumpPitRequest",
        (0x65, 0x02) => "PitBlock",
        (0x65, 0x03) => "PitEnd",
        (0x66, 0x00) => "RequestFileFlash",
        (0x66, 0x02) => "RequestSequence",
        (0x66, 0x03) => "EndSequence",
        (0x67, 0x00) => "EndSession",
        (0x67, 0x01) => "Reboot",
        (0x67, 0x02) => "RebootToOdin",
        (0x67, 0x03) => "Shutdown",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(region: i32, sub: i32) -> Vec<u8> {
        let mut b = vec![0u8; 1024];
        b[0..4].copy_from_slice(&region.to_le_bytes());
        b[4..8].copy_from_slice(&sub.to_le_bytes());
        b
    }

    #[test]
    fn describes_begin_session() {
        let d = describe_command(&cmd(0x64, 0x00));
        assert!(d.contains("BeginSession"), "{d}");
        assert!(d.contains("1024"), "{d}");
    }

    #[test]
    fn describes_end_region_commands() {
        assert!(describe_command(&cmd(0x67, 0x01)).contains("Reboot"));
        assert!(describe_command(&cmd(0x66, 0x03)).contains("EndSequence"));
    }

    #[test]
    fn describes_handshake() {
        assert!(describe_command(b"ODIN").contains("ODIN"));
    }

    #[test]
    fn unknown_command_is_marked() {
        assert!(describe_command(&cmd(0x64, 0x63)).contains('?'));
    }

    #[test]
    fn short_buffer_shows_length_only() {
        assert_eq!(describe_command(&[1, 2, 3]), "[3B]");
    }

    #[test]
    fn hex_preview_truncates_with_ellipsis() {
        assert_eq!(hex_preview(&[0xAA, 0xBB, 0xCC], 2), "aa bb …");
        assert_eq!(hex_preview(&[0x01, 0x02], 4), "01 02");
    }

    #[test]
    fn trace_toggle_round_trips() {
        set_trace(true);
        assert!(trace_enabled());
        set_trace(false);
        assert!(!trace_enabled());
    }
}
