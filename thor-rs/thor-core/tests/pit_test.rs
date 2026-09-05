//! Golden-sample tests for the PIT parser.
//!
//! The fixture `tests/fixtures/sample.pit` is a real PIT pulled from a Galaxy S II
//! (MSM8937 project). The expected values come from the reference C# `printPit` output
//! captured in `dev_files/sample-pit.log`, so these tests pin the port to the original's
//! behavior byte-for-byte.

use thor_core::pit::PitData;

fn sample() -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.pit"))
        .expect("sample.pit fixture must exist")
}

#[test]
fn parses_header_fields() {
    let pit = PitData::parse(&sample()).expect("valid PIT");
    assert_eq!(pit.unknown, "COM_TAR2");
    assert_eq!(pit.project, "MSM8937");
    assert_eq!(pit.reserved, 0);
}

#[test]
fn parses_all_52_entries() {
    let pit = PitData::parse(&sample()).expect("valid PIT");
    assert_eq!(pit.entries.len(), 52);
}

#[test]
fn first_entry_matches_reference_log() {
    let pit = PitData::parse(&sample()).expect("valid PIT");
    let e = &pit.entries[0];
    assert_eq!(e.binary_type, 0, "Binary Type: Phone / AP (0)");
    assert_eq!(e.device_type, 2, "Device Type: EMMC (2)");
    assert_eq!(e.partition_id, 1, "Partition ID: 1");
    assert_eq!(e.attributes, 5, "Partition Type: Data (5)");
    assert_eq!(e.update_attributes, 1, "Filesystem: Basic (1)");
    assert_eq!(e.block_size, 8192, "Start Block: 8192");
    assert_eq!(e.block_count, 1024, "Block Count: 1024");
    assert_eq!(e.partition, "SBL1");
    assert_eq!(e.file_name, "sbl1.mbn");
    assert_eq!(e.delta_name, "");
}

#[test]
fn detects_new_generation_pit() {
    // Entry block sizes differ (8192, 9216, 10240 …) → new-style PIT.
    let pit = PitData::parse(&sample()).expect("valid PIT");
    assert!(pit.is_new_version, "reference log says: Version: v2 (new)");
}

#[test]
fn rejects_wrong_magic() {
    let err = PitData::parse(&[0, 1, 2, 3, 4, 5, 6, 7]).unwrap_err();
    assert!(matches!(err, thor_core::pit::PitError::BadMagic(_)));
}
