//! Pure presentation/safety helpers for the live flash: the progress bar, the
//! human-readable byte formatter, and the "is this a brick-prone partition" check.
//!
//! Kept free of I/O so they can be unit-tested without a device.

/// A fixed-width text progress bar, e.g. `[####------] 40%`.
///
/// `width` is the number of bar cells. A non-positive `total` (nothing to send) renders as
/// full/100%, and `sent` is clamped to `total` so overshoot never draws past the end.
pub fn progress_bar(sent: i64, total: i64, width: usize) -> String {
    let frac = if total <= 0 {
        1.0
    } else {
        (sent.clamp(0, total) as f64) / (total as f64)
    };
    let filled = (frac * width as f64).round() as usize;
    let filled = filled.min(width);
    let pct = (frac * 100.0).round() as i64;
    format!(
        "[{}{}] {pct}%",
        "#".repeat(filled),
        "-".repeat(width - filled)
    )
}

/// Whether flashing `name` wrong tends to *hard*-brick a device (the bootloader chain plus
/// the partition/boot-config tables). Used to gate an extra confirmation. Case-insensitive
/// substring match against a curated keyword list.
pub fn is_critical_partition(name: &str) -> bool {
    // The bootloader chain (SBL/XBL/ABOOT/BOOTLOADER), plus the boot-config and partition
    // tables (PARAM/PIT/GPT) and the encryption filesystem (EFS): flashing any of these
    // wrong is the classic way to hard-brick. Substring match, case-insensitive.
    const CRITICAL: &[&str] = &[
        "BOOTLOADER",
        "SBL",
        "XBL",
        "ABOOT",
        "PARAM",
        "PIT",
        "GPT",
        "EFS",
    ];
    let upper = name.to_ascii_uppercase();
    CRITICAL.iter().any(|k| upper.contains(k))
}

/// Format a byte count as B / KiB / MiB / GiB with one decimal for the scaled units.
pub fn human_bytes(n: i64) -> String {
    const KIB: f64 = 1024.0;
    let f = n as f64;
    if f < KIB {
        format!("{n} B")
    } else if f < KIB * KIB {
        format!("{:.1} KiB", f / KIB)
    } else if f < KIB * KIB * KIB {
        format!("{:.1} MiB", f / (KIB * KIB))
    } else {
        format!("{:.1} GiB", f / (KIB * KIB * KIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_bar_empty_is_zero_percent() {
        assert_eq!(progress_bar(0, 100, 10), "[----------] 0%");
    }

    #[test]
    fn progress_bar_partial_fills_proportionally() {
        assert_eq!(progress_bar(40, 100, 10), "[####------] 40%");
    }

    #[test]
    fn progress_bar_full_is_hundred_percent() {
        assert_eq!(progress_bar(100, 100, 10), "[##########] 100%");
    }

    #[test]
    fn progress_bar_overshoot_clamps_to_full() {
        // sent past total (padding on the last sequence) must not draw past the end.
        assert_eq!(progress_bar(150, 100, 10), "[##########] 100%");
    }

    #[test]
    fn progress_bar_zero_total_is_full() {
        // A zero-length flash has nothing to do — show it done, not a divide-by-zero.
        assert_eq!(progress_bar(0, 0, 10), "[##########] 100%");
    }

    #[test]
    fn critical_partitions_are_flagged() {
        for name in ["BOOTLOADER", "SBL1", "xbl", "ABOOT", "PARAM", "PIT"] {
            assert!(is_critical_partition(name), "{name} should be critical");
        }
    }

    #[test]
    fn ordinary_partitions_are_not_flagged() {
        for name in ["BOOT", "RECOVERY", "SYSTEM", "USERDATA", "CACHE"] {
            assert!(
                !is_critical_partition(name),
                "{name} should not be critical"
            );
        }
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(1_048_576), "1.0 MiB");
        assert_eq!(human_bytes(1_610_612_736), "1.5 GiB");
    }
}
