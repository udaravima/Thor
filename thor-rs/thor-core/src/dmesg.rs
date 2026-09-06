//! Carve the kernel `printk`/`dmesg` log out of a RAM dump — the payoff of the upload-mode
//! dumper, and the "USB → printk" goal.
//!
//! The kernel log is a ring buffer *in RAM*, so once [`upload`](crate::upload) has dumped
//! memory, the log can be recovered offline. This parses the **structured `printk_log` record
//! format** used by Linux 3.5–5.9 (which covers the 2016–2019 Samsung devices of interest,
//! e.g. the J2 2018's ~3.18 kernel). Each record is:
//!
//! ```text
//! offset 0  u64 ts_nsec     timestamp (ns)
//! offset 8  u16 len         total record length (header + text + dict + padding)
//! offset 10 u16 text_len    length of the message text
//! offset 12 u16 dict_len    length of the key/value dict (ignored here)
//! offset 14 u8  facility
//! offset 15 u8  flags:5 | level:3
//! offset 16 …  text         (text_len bytes), then dict, then padding to `len`
//! ```
//!
//! A record with `len == 0` marks the ring wrap / end. The newest kernels (5.10+) use a
//! different lockless `printk_ringbuffer`, which is **not** handled here yet.

/// Fixed size of a `printk_log` record header.
const HEADER: usize = 16;

/// One decoded kernel-log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// Monotonic timestamp in nanoseconds.
    pub ts_nsec: u64,
    /// Syslog level (0–7), best-effort.
    pub level: u8,
    /// The message text (trailing newline trimmed).
    pub text: String,
}

impl LogRecord {
    /// Render like `dmesg`: `[    1.234567] message`.
    pub fn format_line(&self) -> String {
        let secs = self.ts_nsec / 1_000_000_000;
        let frac = (self.ts_nsec % 1_000_000_000) / 1000; // microseconds
        format!("[{secs:5}.{frac:06}] {}", self.text)
    }
}

/// Parse consecutive `printk_log` records starting at offset 0 of `buf`, stopping at the ring
/// wrap (`len == 0`), an invalid record, or the end of the buffer. Returns the records and the
/// number of bytes consumed.
fn parse_from(buf: &[u8]) -> (Vec<LogRecord>, usize) {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + HEADER <= buf.len() {
        let len = u16::from_le_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        if len == 0 {
            break; // ring wrap / end marker
        }
        let text_len = u16::from_le_bytes([buf[pos + 10], buf[pos + 11]]) as usize;
        // Bounds and self-consistency: don't read past the buffer or the record.
        if len < HEADER || pos + len > buf.len() || HEADER + text_len > len {
            break;
        }
        let ts = u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap());
        let text = String::from_utf8_lossy(&buf[pos + HEADER..pos + HEADER + text_len])
            .trim_end_matches('\n')
            .to_string();
        let level = buf[pos + 15] & 0x07;
        out.push(LogRecord {
            ts_nsec: ts,
            level,
            text,
        });
        pos += len;
    }
    (out, pos)
}

/// Parse `printk_log` records starting at offset 0 of `buf`.
pub fn parse_printk_records(buf: &[u8]) -> Vec<LogRecord> {
    parse_from(buf).0
}

/// Scan a RAM dump for the kernel log buffer and carve out the records. Heuristic: it tries
/// every offset that looks like a plausible record header (a cheap check, so this stays O(n))
/// and keeps the longest run of valid records found. Byte-granular, so the log is found
/// wherever it sits in the dump. Returns empty if nothing convincing turns up.
pub fn carve_dmesg(dump: &[u8]) -> Vec<LogRecord> {
    // Require a decent run so random data that briefly looks record-shaped isn't mistaken for
    // a log.
    const MIN_RUN: usize = 5;
    let mut best: Vec<LogRecord> = Vec::new();
    let mut pos = 0;
    while pos + HEADER <= dump.len() {
        if plausible_header(&dump[pos..]) {
            let (recs, consumed) = parse_from(&dump[pos..]);
            let n = recs.len();
            if n > best.len() {
                best = recs;
            }
            if n >= 2 {
                pos += consumed.max(1); // skip past this run
                continue;
            }
        }
        pos += 1;
    }
    if best.len() >= MIN_RUN {
        best
    } else {
        Vec::new()
    }
}

/// Cheap check that `buf` starts with a plausible `printk_log` record: sane lengths and mostly
/// printable text. Keeps the full parse from running at every junk offset.
fn plausible_header(buf: &[u8]) -> bool {
    if buf.len() < HEADER {
        return false;
    }
    let len = u16::from_le_bytes([buf[8], buf[9]]) as usize;
    let text_len = u16::from_le_bytes([buf[10], buf[11]]) as usize;
    if len < HEADER || text_len == 0 || HEADER + text_len > len || len > buf.len() {
        return false;
    }
    let text = &buf[HEADER..HEADER + text_len];
    let printable = text
        .iter()
        .filter(|&&b| b == b'\n' || (0x20..=0x7e).contains(&b))
        .count();
    printable * 10 >= text.len() * 9 // ≥90% printable ASCII
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one `printk_log` record (dict_len 0, padded to 8).
    fn record(ts: u64, level: u8, text: &str) -> Vec<u8> {
        let total = HEADER + text.len();
        let padded = total.div_ceil(8) * 8;
        let mut r = vec![0u8; padded];
        r[0..8].copy_from_slice(&ts.to_le_bytes());
        r[8..10].copy_from_slice(&(padded as u16).to_le_bytes());
        r[10..12].copy_from_slice(&(text.len() as u16).to_le_bytes());
        // dict_len (12..14) = 0, facility (14) = 0
        r[15] = level & 0x07;
        r[HEADER..HEADER + text.len()].copy_from_slice(text.as_bytes());
        r
    }

    fn log_buffer(records: &[Vec<u8>]) -> Vec<u8> {
        let mut buf = Vec::new();
        for r in records {
            buf.extend_from_slice(r);
        }
        buf.extend_from_slice(&[0u8; HEADER]); // zero-len terminator
        buf
    }

    #[test]
    fn parses_consecutive_records() {
        let buf = log_buffer(&[
            record(1_000_000, 6, "Linux version 3.18.14"),
            record(1_500_000_000, 3, "boot: something failed"),
        ]);
        let recs = parse_printk_records(&buf);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].ts_nsec, 1_000_000);
        assert_eq!(recs[0].text, "Linux version 3.18.14");
        assert_eq!(recs[1].text, "boot: something failed");
    }

    #[test]
    fn stops_at_zero_length_record() {
        let buf = log_buffer(&[record(1, 6, "only one")]);
        assert_eq!(parse_printk_records(&buf).len(), 1);
    }

    #[test]
    fn rejects_record_claiming_length_past_buffer() {
        // A header claiming len = 4096 in a tiny buffer must not read out of bounds.
        let mut buf = vec![0u8; 32];
        buf[8..10].copy_from_slice(&4096u16.to_le_bytes());
        buf[10..12].copy_from_slice(&10u16.to_le_bytes());
        assert!(parse_printk_records(&buf).is_empty());
    }

    #[test]
    fn format_line_looks_like_dmesg() {
        let r = LogRecord {
            ts_nsec: 1_234_567_000,
            level: 6,
            text: "hello".into(),
        };
        assert_eq!(r.format_line(), "[    1.234567] hello");
    }

    #[test]
    fn carves_a_log_embedded_in_junk() {
        // random-ish junk, then a real log run, then more junk.
        let mut dump = vec![0xABu8; 500];
        let log = log_buffer(&[
            record(1_000, 6, "carve me: line one of the kernel log"),
            record(2_000, 6, "carve me: line two of the kernel log"),
            record(3_000, 4, "carve me: line three, a warning"),
            record(4_000, 6, "carve me: line four"),
            record(5_000, 6, "carve me: line five"),
            record(6_000, 6, "carve me: line six"),
            record(7_000, 6, "carve me: line seven"),
            record(8_000, 6, "carve me: line eight"),
        ]);
        dump.extend_from_slice(&log);
        dump.extend_from_slice(&[0xCDu8; 500]);

        let recs = carve_dmesg(&dump);
        assert!(
            recs.len() >= 8,
            "expected to carve the run, got {}",
            recs.len()
        );
        assert_eq!(recs[0].text, "carve me: line one of the kernel log");
    }

    #[test]
    fn carve_returns_empty_for_pure_junk() {
        assert!(carve_dmesg(&[0x55u8; 4096]).is_empty());
    }

    #[test]
    fn carves_log_at_an_unaligned_offset() {
        // The log buffer can sit at any byte offset in a dump — 777 is not 4-aligned.
        let mut dump = vec![0xABu8; 777];
        dump.extend_from_slice(&log_buffer(&[
            record(1_000, 6, "unaligned log line one"),
            record(2_000, 6, "unaligned log line two"),
            record(3_000, 6, "unaligned log line three"),
            record(4_000, 6, "unaligned log line four"),
            record(5_000, 6, "unaligned log line five"),
        ]));
        let recs = carve_dmesg(&dump);
        assert_eq!(recs.len(), 5);
        assert_eq!(recs[0].text, "unaligned log line one");
    }
}
