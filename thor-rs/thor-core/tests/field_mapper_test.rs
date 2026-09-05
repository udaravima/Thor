//! Golden tests for PIT field labelling, pinned to the reference `printPit` output in
//! `dev_files/sample-pit.log` (a new-generation PIT).

use thor_core::pit::FieldMapper;

#[test]
fn new_generation_labels_match_reference_log() {
    let m = FieldMapper::for_version(true);
    assert_eq!(m.binary_type.label, "Binary Type");
    assert_eq!(m.binary_type.describe(0), "Phone / AP");
    assert_eq!(m.device_type.label, "Device Type");
    assert_eq!(m.device_type.describe(2), "EMMC");
    assert_eq!(m.attributes.label, "Partition Type");
    assert_eq!(m.attributes.describe(5), "Data");
    assert_eq!(m.update_attributes.label, "Filesystem");
    assert_eq!(m.update_attributes.describe(1), "Basic");
    assert_eq!(m.block_size_label, "Start Block");
    assert_eq!(m.block_count_label, "Block Count");
}

#[test]
fn old_generation_uses_old_labels() {
    let m = FieldMapper::for_version(false);
    assert_eq!(m.attributes.label, "Attributes");
    assert_eq!(m.attributes.describe(0), "Read-only");
    assert_eq!(m.update_attributes.label, "Update Attributes");
    assert_eq!(m.block_size_label, "Block Size");
    assert_eq!(m.device_type.describe(2), "MoviNAND");
}

#[test]
fn out_of_range_value_is_unknown_not_a_panic() {
    // The original C# GetMapping had an off-by-one that could index past the array.
    // The port must return "Unknown" for any out-of-range value — including negatives.
    let m = FieldMapper::for_version(true);
    assert_eq!(m.device_type.describe(999), "Unknown");
    assert_eq!(m.device_type.describe(-1), "Unknown");
}
