//! The flash-sequence math — how a partition image is cut into ~30 MB *sequences*, each
//! into fixed-size *parts*, driven by constants chosen from the bootloader version.
//!
//! This is pure planning logic, deliberately separated from the USB I/O so it can be
//! tested exhaustively without a device. It is the single easiest place to introduce a
//! subtle bug, so it is pinned by vectors for both bootloader generations. See
//! `../../docs/odin-protocol.md#the-flash-sequence-math-the-part-that-bites`.

/// Sizing/timeout constants for a flash, selected from the bootloader version returned by
/// `BeginSession`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashParams {
    /// Bytes per USB part (128 KiB on old bootloaders, 1 MiB on new).
    pub packet_size: i64,
    /// Parts per sequence (240 old, 30 new — both ≈ 30 MB per sequence).
    pub packets_per_sequence: i64,
    /// How long to wait for the device to commit a sequence.
    pub flash_timeout_ms: u64,
}

impl FlashParams {
    /// Constants for a bootloader `version` (from the `BeginSession` reply).
    ///
    /// The original C# only handled `0|1` and `>=2` and left the fields at zero for other
    /// values; this treats anything `< 2` as the old generation, which is defensive against
    /// an unexpected version without changing behavior for real devices (0, 1, 2, 3…).
    pub fn for_bootloader_version(version: i16) -> Self {
        if version >= 2 {
            FlashParams {
                packet_size: 1_048_576,
                packets_per_sequence: 30,
                flash_timeout_ms: 120_000,
            }
        } else {
            FlashParams {
                packet_size: 131_072,
                packets_per_sequence: 240,
                flash_timeout_ms: 30_000,
            }
        }
    }

    /// Bytes in one full sequence (`packet_size * packets_per_sequence`).
    pub fn sequence_size(&self) -> i64 {
        self.packet_size * self.packets_per_sequence
    }
}

/// One planned sequence of a partition flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashSequence {
    pub index: usize,
    /// True byte count of this sequence's data (declared to the device on end-of-sequence).
    pub real_size: i64,
    /// `real_size` rounded up to a whole number of packets (what's actually sent on the wire).
    pub aligned_size: i64,
    /// Number of `packet_size` parts sent for this sequence.
    pub parts: i64,
    pub is_last: bool,
}

/// Plan how a `length`-byte image is split into sequences and parts.
///
/// Returns an empty plan for a zero-length image. Uses `i64` throughout to avoid the
/// original C#'s 32-bit `totalBytes` overflow on partitions larger than 2 GiB.
pub fn plan_flash(length: i64, params: &FlashParams) -> Vec<FlashSequence> {
    let seq = params.sequence_size();
    if length <= 0 || seq <= 0 || params.packet_size <= 0 {
        return Vec::new();
    }

    // How many whole sequences, plus a possible partial final one.
    let mut count = length / seq;
    let remainder = length % seq;
    let last_size = if remainder != 0 {
        count += 1;
        remainder
    } else {
        seq
    };

    let packet = params.packet_size;
    (0..count)
        .map(|i| {
            let is_last = i + 1 == count;
            let real_size = if is_last { last_size } else { seq };
            // Round the on-wire size up to a whole number of packets.
            let aligned_size = match real_size % packet {
                0 => real_size,
                rem => real_size + (packet - rem),
            };
            FlashSequence {
                index: i as usize,
                real_size,
                aligned_size,
                parts: aligned_size / packet,
                is_last,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_params() -> FlashParams {
        FlashParams {
            packet_size: 131_072,
            packets_per_sequence: 240,
            flash_timeout_ms: 30_000,
        }
    }
    fn new_params() -> FlashParams {
        FlashParams {
            packet_size: 1_048_576,
            packets_per_sequence: 30,
            flash_timeout_ms: 120_000,
        }
    }

    #[test]
    fn params_from_bootloader_version() {
        assert_eq!(FlashParams::for_bootloader_version(0), old_params());
        assert_eq!(FlashParams::for_bootloader_version(1), old_params());
        assert_eq!(FlashParams::for_bootloader_version(2), new_params());
        assert_eq!(FlashParams::for_bootloader_version(3), new_params());
    }

    #[test]
    fn both_generations_use_30mb_sequences() {
        // The trap: same ~30 MB sequence, different packetization.
        assert_eq!(old_params().sequence_size(), 31_457_280);
        assert_eq!(new_params().sequence_size(), 31_457_280);
    }

    #[test]
    fn exact_single_sequence_new() {
        let plan = plan_flash(31_457_280, &new_params());
        assert_eq!(plan.len(), 1);
        assert_eq!(
            plan[0],
            FlashSequence {
                index: 0,
                real_size: 31_457_280,
                aligned_size: 31_457_280,
                parts: 30,
                is_last: true,
            }
        );
    }

    #[test]
    fn one_sequence_plus_a_tail_new() {
        // 30 MB + 500 bytes → two sequences; the tail's last part is zero-padded to 1 MiB.
        let plan = plan_flash(31_457_280 + 500, &new_params());
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].real_size, 31_457_280);
        assert_eq!(plan[0].parts, 30);
        assert!(!plan[0].is_last);
        assert_eq!(plan[1].real_size, 500);
        assert_eq!(plan[1].aligned_size, 1_048_576); // padded up to one packet
        assert_eq!(plan[1].parts, 1);
        assert!(plan[1].is_last);
    }

    #[test]
    fn tiny_image_is_one_padded_part() {
        let plan = plan_flash(1500, &new_params());
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].real_size, 1500);
        assert_eq!(plan[0].aligned_size, 1_048_576);
        assert_eq!(plan[0].parts, 1);
        assert!(plan[0].is_last);
    }

    #[test]
    fn partial_tail_aligns_up_old_generation() {
        // Old params (128 KiB packets): 30 MB + 200000 bytes.
        let plan = plan_flash(31_457_280 + 200_000, &old_params());
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].parts, 240);
        assert!(!plan[0].is_last);
        assert_eq!(plan[1].real_size, 200_000);
        assert_eq!(plan[1].aligned_size, 262_144); // 2 × 131072
        assert_eq!(plan[1].parts, 2);
        assert!(plan[1].is_last);
    }

    #[test]
    fn zero_length_is_empty_plan() {
        assert!(plan_flash(0, &new_params()).is_empty());
    }
}
