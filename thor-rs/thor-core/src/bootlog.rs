//! One-step **boot-log** extraction: carve the kernel `printk` log out of the RAM region that
//! Samsung's `sec_log` keeps it in, so a caller doesn't need to know the magic address.
//!
//! This ties [`upload`](crate::upload) (the SUC RAM dumper) to [`dmesg`](crate::dmesg) (the
//! printk carver). The dump itself lives in `upload`; this module owns the *pure* part — the
//! default `sec_log` location and the "structured records, else 5.10+ ringbuffer text" fallback
//! — so it is fully unit-testable without a device.
//!
//! The kernel copies its log into a fixed RAM buffer even when the live console is silenced
//! (`console=null`). On the SM-J250Y (Qualcomm) the kernel cmdline advertises it as
//! `sec_log=0x200000@0x85200000`, i.e. 2 MiB at physical `0x85200000` — the defaults below.
//! Both are board-specific and overridable.

use crate::dmesg::{self, LogRecord};

/// Default physical address of the kernel-log (`sec_log`) RAM buffer, from the SM-J250Y kernel
/// cmdline (`sec_log=0x200000@0x85200000`). Board-specific — override for other devices.
pub const DEFAULT_SEC_LOG_ADDR: u64 = 0x8520_0000;
/// Default size of that buffer (2 MiB on SM-J250Y).
pub const DEFAULT_SEC_LOG_SIZE: u64 = 0x0020_0000;

/// A carved boot log: structured records (with timestamps) when the classic `printk_log` format
/// is found, else best-effort text lines from a Linux 5.10+ `printk_ringbuffer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootLog {
    /// Structured `printk_log` records (Linux 3.5–5.9): timestamp + level + text.
    Records(Vec<LogRecord>),
    /// Text-only lines recovered from a 5.10+ ringbuffer (no timestamps).
    TextLines(Vec<String>),
}

impl BootLog {
    /// True if no log line was recovered.
    pub fn is_empty(&self) -> bool {
        match self {
            BootLog::Records(r) => r.is_empty(),
            BootLog::TextLines(t) => t.is_empty(),
        }
    }

    /// Number of recovered lines.
    pub fn len(&self) -> usize {
        match self {
            BootLog::Records(r) => r.len(),
            BootLog::TextLines(t) => t.len(),
        }
    }

    /// A short human description of which format was recovered.
    pub fn kind(&self) -> &'static str {
        match self {
            BootLog::Records(_) => "structured printk_log",
            BootLog::TextLines(_) => "5.10+ ringbuffer — text only, no timestamps",
        }
    }

    /// Render as `dmesg`-style lines: records carry `[ts] text`, ringbuffer text is passed
    /// through as-is.
    pub fn lines(&self) -> Vec<String> {
        match self {
            BootLog::Records(r) => r.iter().map(LogRecord::format_line).collect(),
            BootLog::TextLines(t) => t.clone(),
        }
    }
}

/// Carve a kernel log out of already-dumped RAM: prefer the structured `printk_log` records; if
/// none are found, fall back to the 5.10+ ringbuffer text carve. Pure (no I/O).
pub fn carve(raw: &[u8]) -> BootLog {
    let recs = dmesg::carve_dmesg(raw);
    if !recs.is_empty() {
        BootLog::Records(recs)
    } else {
        BootLog::TextLines(dmesg::carve_ringbuffer(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HDR: usize = 16;

    /// Build one `printk_log` record (dict_len 0, padded to 8) — mirrors the on-wire layout.
    fn record(ts: u64, level: u8, text: &str) -> Vec<u8> {
        let padded = (HDR + text.len()).div_ceil(8) * 8;
        let mut r = vec![0u8; padded];
        r[0..8].copy_from_slice(&ts.to_le_bytes());
        r[8..10].copy_from_slice(&(padded as u16).to_le_bytes());
        r[10..12].copy_from_slice(&(text.len() as u16).to_le_bytes());
        r[15] = level & 0x07;
        r[HDR..HDR + text.len()].copy_from_slice(text.as_bytes());
        r
    }

    /// A printk_log buffer of `n` records embedded in junk (so it exercises the carver, not just
    /// the parser).
    fn printk_buffer() -> Vec<u8> {
        let mut buf = vec![0xABu8; 128];
        for i in 0..6 {
            buf.extend_from_slice(&record(1_000 * (i + 1), 6, &format!("boot line {i}")));
        }
        buf.extend_from_slice(&[0u8; HDR]); // terminator
        buf.extend_from_slice(&[0xCDu8; 64]);
        buf
    }

    /// One 5.10 data block `[u64 id][text]`, padded to 8.
    fn data_block(id: u64, text: &str) -> Vec<u8> {
        let padded = (8 + text.len()).div_ceil(8) * 8;
        let mut b = vec![0u8; padded];
        b[0..8].copy_from_slice(&id.to_le_bytes());
        b[8..8 + text.len()].copy_from_slice(text.as_bytes());
        b
    }

    #[test]
    fn carve_returns_records_for_printk_log() {
        let log = carve(&printk_buffer());
        match log {
            BootLog::Records(ref r) => {
                assert!(r.len() >= 6, "expected >=6 records, got {}", r.len());
                assert_eq!(r[0].text, "boot line 0");
            }
            other => panic!("expected Records, got {other:?}"),
        }
        assert!(!log.is_empty());
        assert_eq!(log.len(), log.lines().len());
    }

    #[test]
    fn carve_falls_back_to_textlines_for_ringbuffer() {
        // No valid printk_log records here, only 5.10 data blocks.
        let mut dump = vec![0u8; 64];
        dump.extend(data_block(1, "ring: alpha"));
        dump.extend(data_block(2, "ring: bravo"));
        dump.extend(data_block(3, "ring: charlie"));
        dump.extend(vec![0xFFu8; 32]);

        let log = carve(&dump);
        match log {
            BootLog::TextLines(ref t) => {
                assert!(t.iter().any(|l| l == "ring: alpha"), "{t:?}");
                assert!(t.iter().any(|l| l == "ring: charlie"), "{t:?}");
            }
            other => panic!("expected TextLines, got {other:?}"),
        }
    }

    #[test]
    fn carve_is_empty_for_junk() {
        assert!(carve(&[0x55u8; 4096]).is_empty());
    }

    #[test]
    fn records_render_as_dmesg_lines() {
        let log = carve(&printk_buffer());
        let first = &log.lines()[0];
        assert!(
            first.starts_with('['),
            "dmesg line should start with '[': {first}"
        );
        assert!(first.contains("boot line 0"));
    }

    #[test]
    fn default_sec_log_region_is_the_j250y_buffer() {
        // Guard against accidental edits to the documented defaults.
        assert_eq!(DEFAULT_SEC_LOG_ADDR, 0x8520_0000);
        assert_eq!(DEFAULT_SEC_LOG_SIZE, 2 * 1024 * 1024);
    }
}
