//! PIT (Partition Information Table) parsing.
//!
//! A PIT is Samsung's map of the phone's storage: which partitions exist, how big they
//! are, and what filename each expects. Thor needs it because the flash protocol
//! addresses partitions by numeric id and type, not by name.
//!
//! Binary layout (little-endian, fixed-width, no padding):
//! - Header, 28 bytes: magic `0x12349876`, entry count, 8-byte `unknown` string,
//!   8-byte `project` string, reserved i32.
//! - Then `count` entries of 132 bytes each: nine i32 fields, then three 32-byte
//!   ASCII strings (`partition`, `file_name`, `delta_name`).
//!
//! See `../../docs/pit-format.md`.

/// Magic number every PIT begins with.
pub const PIT_MAGIC: u32 = 0x1234_9876;

const HEADER_LEN: usize = 28;
const ENTRY_LEN: usize = 132;

/// One partition's row in the PIT.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PitEntry {
    pub binary_type: i32,
    pub device_type: i32,
    pub partition_id: i32,
    pub attributes: i32,
    pub update_attributes: i32,
    /// Block size on old PITs; repurposed as the partition's start block on new ones.
    pub block_size: i32,
    pub block_count: i32,
    pub file_offset: i32,
    pub file_size: i32,
    pub partition: String,
    pub file_name: String,
    pub delta_name: String,
}

/// A parsed PIT: header fields plus every entry.
#[derive(Debug, Default, Clone)]
pub struct PitData {
    pub entries: Vec<PitEntry>,
    /// True when the `block_size` field varies between entries — the heuristic the
    /// original Thor uses to distinguish new-generation PITs from old ones.
    pub is_new_version: bool,
    pub unknown: String,
    pub project: String,
    pub reserved: i32,
}

/// Why a PIT failed to parse.
#[derive(Debug, PartialEq, Eq)]
pub enum PitError {
    /// First four bytes were not [`PIT_MAGIC`].
    BadMagic(u32),
    /// The buffer ended before a declared field could be read.
    Truncated,
}

impl std::fmt::Display for PitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PitError::BadMagic(m) => write!(f, "magic number mismatch: got 0x{m:08X}"),
            PitError::Truncated => write!(f, "PIT data ended unexpectedly"),
        }
    }
}

impl std::error::Error for PitError {}

impl PitData {
    /// Parse a PIT from raw bytes (a `.pit` file or a live `DumpPIT` result).
    ///
    /// Trailing bytes after the last declared entry are ignored — live dumps are padded
    /// to a block boundary.
    pub fn parse(bytes: &[u8]) -> Result<PitData, PitError> {
        let mut r = Cursor::new(bytes);

        let magic = r.u32()?;
        if magic != PIT_MAGIC {
            return Err(PitError::BadMagic(magic));
        }
        let count = r.i32()?;
        let unknown = r.fixed_string(8)?;
        let project = r.fixed_string(8)?;
        let reserved = r.i32()?;

        let mut entries = Vec::with_capacity(count.max(0) as usize);
        let mut is_new_version = false;
        let mut last_block_size = 0i32;
        for i in 0..count {
            let entry = PitEntry {
                binary_type: r.i32()?,
                device_type: r.i32()?,
                partition_id: r.i32()?,
                attributes: r.i32()?,
                update_attributes: r.i32()?,
                block_size: r.i32()?,
                block_count: r.i32()?,
                file_offset: r.i32()?,
                file_size: r.i32()?,
                partition: r.fixed_string(32)?,
                file_name: r.fixed_string(32)?,
                delta_name: r.fixed_string(32)?,
            };
            if i > 0 && last_block_size != entry.block_size {
                is_new_version = true;
            }
            last_block_size = entry.block_size;
            entries.push(entry);
        }

        Ok(PitData {
            entries,
            is_new_version,
            unknown,
            project,
            reserved,
        })
    }
}

/// One PIT field's human-readable labelling: the field's own name plus the descriptions
/// for each numeric value it can hold.
///
/// Unlike the original C# (where the field name lived at index 0 of the value array and
/// callers indexed `value + 1`), here `label` is separate and `describe(value)` indexes
/// the descriptions directly — which also removes the original's off-by-one bounds bug.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub label: &'static str,
    values: &'static [&'static str],
}

impl Field {
    /// Describe a raw value, or `"Unknown"` if it's out of range (never panics).
    pub fn describe(&self, value: i32) -> &'static str {
        usize::try_from(value)
            .ok()
            .and_then(|i| self.values.get(i))
            .copied()
            .unwrap_or("Unknown")
    }
}

/// The set of labels/descriptions used to render a PIT, which differs between the old and
/// new PIT generations.
#[derive(Debug, Clone, Copy)]
pub struct FieldMapper {
    pub binary_type: Field,
    pub device_type: Field,
    pub attributes: Field,
    pub update_attributes: Field,
    pub block_size_label: &'static str,
    pub block_count_label: &'static str,
}

impl FieldMapper {
    /// Pick the mapper matching a PIT's generation (`is_new_version`).
    pub fn for_version(is_new: bool) -> &'static FieldMapper {
        if is_new {
            &NEW_PIT_MAPPER
        } else {
            &OLD_PIT_MAPPER
        }
    }
}

/// Labels/descriptions for new-generation PITs.
static NEW_PIT_MAPPER: FieldMapper = FieldMapper {
    binary_type: Field {
        label: "Binary Type",
        values: &["Phone / AP", "Modem / CP"],
    },
    device_type: Field {
        label: "Device Type",
        values: &["OneNAND", "NAND", "EMMC", "SPI", "IDE", "NAND X16"],
    },
    attributes: Field {
        label: "Partition Type",
        values: &[
            "None",
            "BCT",
            "Bootloader",
            "Partition Table",
            "NV-Data",
            "Data",
            "MBR",
            "EBR",
            "GP1",
            "GP1",
        ],
    },
    update_attributes: Field {
        label: "Filesystem",
        values: &["None", "Basic", "Enhanced", "EXT2", "YAFFS2", "EXT4"],
    },
    block_size_label: "Start Block",
    block_count_label: "Block Count",
};

/// Labels/descriptions for old-generation PITs.
static OLD_PIT_MAPPER: FieldMapper = FieldMapper {
    binary_type: Field {
        label: "Binary Type",
        values: &["Phone / AP", "Modem / CP"],
    },
    device_type: Field {
        label: "Device Type",
        values: &["OneNAND", "NAND", "MoviNAND"],
    },
    attributes: Field {
        label: "Attributes",
        values: &["Read-only", "Read-write", "STL"],
    },
    update_attributes: Field {
        label: "Update Attributes",
        values: &["None", "FOTA", "Secure", "Secure FOTA"],
    },
    block_size_label: "Block Size",
    block_count_label: "Block Count",
};

/// A minimal little-endian byte reader that bounds-checks every read.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], PitError> {
        let end = self.pos.checked_add(n).ok_or(PitError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(PitError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, PitError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, PitError> {
        Ok(self.u32()? as i32)
    }

    /// Read `n` bytes as an ASCII string, trimming everything from the first NUL.
    fn fixed_string(&mut self, n: usize) -> Result<String, PitError> {
        let b = self.take(n)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Ok(String::from_utf8_lossy(&b[..end]).into_owned())
    }
}

const _: () = {
    // Compile-time reminders that the on-wire sizes match the layout doc.
    assert!(HEADER_LEN == 28);
    assert!(ENTRY_LEN == 132);
};
