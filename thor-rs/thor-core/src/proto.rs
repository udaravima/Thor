//! Low-level Odin wire helpers: building the fixed 1024-byte command packets and reading
//! little-endian integers back out of replies.
//!
//! Every Odin command is a 1024-byte buffer whose first i32 is a *region* and whose second
//! is a *sub-command*, with arguments packed after byte 8 — all little-endian. Replies are
//! short (usually 8 bytes) and carry a return value at bytes 4..8. See
//! `../../docs/odin-protocol.md`.

/// The fixed size of every Odin command packet.
pub const PACKET_LEN: usize = 1024;

/// A builder for a single 1024-byte Odin command packet.
///
/// Fields are written little-endian at explicit offsets, mirroring the original C#
/// `new byte[1024]` + `WriteInt/WriteLong/WriteString` pattern, but with bounds checking.
#[derive(Clone)]
pub struct Packet {
    buf: [u8; PACKET_LEN],
}

impl Default for Packet {
    fn default() -> Self {
        Self::new()
    }
}

impl Packet {
    /// A zero-filled packet.
    pub fn new() -> Self {
        Packet {
            buf: [0u8; PACKET_LEN],
        }
    }

    /// A packet with `region` at offset 0 and `sub_command` at offset 4 — the header every
    /// Odin command shares.
    pub fn command(region: i32, sub_command: i32) -> Self {
        let mut p = Packet::new();
        p.write_i32(0, region).write_i32(4, sub_command);
        p
    }

    /// Write a little-endian i32 at `offset`.
    ///
    /// # Panics
    /// If `offset + 4` exceeds [`PACKET_LEN`] — offsets are compile-time constants in
    /// protocol code, so an overflow is a bug, not a runtime condition.
    pub fn write_i32(&mut self, offset: usize, value: i32) -> &mut Self {
        self.buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        self
    }

    /// Write a little-endian i64 at `offset`.
    ///
    /// # Panics
    /// If `offset + 8` exceeds [`PACKET_LEN`].
    pub fn write_i64(&mut self, offset: usize, value: i64) -> &mut Self {
        self.buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        self
    }

    /// Write ASCII `text` starting at `offset` (no length prefix, no NUL terminator added).
    ///
    /// # Panics
    /// If the text would run past [`PACKET_LEN`].
    pub fn write_str(&mut self, offset: usize, text: &str) -> &mut Self {
        let bytes = text.as_bytes();
        self.buf[offset..offset + bytes.len()].copy_from_slice(bytes);
        self
    }

    /// The finished 1024-byte packet.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }
}

/// Read a little-endian i32 at `offset`, or `None` if the buffer is too short.
///
/// Used to pull return values out of device replies (e.g. the bootloader version at
/// bytes 4..8 of a `BeginSession` ack).
pub fn read_i32_le(buf: &[u8], offset: usize) -> Option<i32> {
    let slice = buf.get(offset..offset + 4)?;
    Some(i32::from_le_bytes(slice.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_sets_region_and_subcommand() {
        let p = Packet::command(0x64, 0x05);
        assert_eq!(&p.as_bytes()[0..4], &0x64i32.to_le_bytes());
        assert_eq!(&p.as_bytes()[4..8], &0x05i32.to_le_bytes());
    }

    #[test]
    fn packet_is_always_1024_bytes_and_zero_padded() {
        let mut p = Packet::command(0x64, 0x00);
        p.write_i32(8, i32::MAX);
        assert_eq!(p.as_bytes().len(), 1024);
        // everything past the written fields stays zero
        assert!(p.as_bytes()[12..].iter().all(|&b| b == 0));
    }

    #[test]
    fn write_i32_is_little_endian_at_offset() {
        let mut p = Packet::new();
        p.write_i32(8, 0x0102_0304);
        assert_eq!(&p.as_bytes()[8..12], &[0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn write_i64_is_little_endian_at_offset() {
        // SetTotalBytes packs an 8-byte total at offset 8.
        let mut p = Packet::new();
        p.write_i64(8, 0x0102_0304_0506_0708);
        assert_eq!(
            &p.as_bytes()[8..16],
            &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
    }

    #[test]
    fn write_str_writes_ascii_at_offset() {
        // SetRegionCode writes a 3-char code at offset 8.
        let mut p = Packet::new();
        p.write_str(8, "XAA");
        assert_eq!(&p.as_bytes()[8..11], b"XAA");
        assert_eq!(p.as_bytes()[11], 0); // nothing written past the string
    }

    #[test]
    fn builder_methods_chain() {
        let mut p = Packet::command(0x66, 0x03);
        p.write_i32(8, 0x00).write_i32(12, 4096).write_i32(16, 1);
        assert_eq!(read_i32_le(p.as_bytes(), 12), Some(4096));
    }

    #[test]
    fn read_i32_le_reads_reply_value() {
        let reply = [0u8, 0, 0, 0, 0x2A, 0, 0, 0]; // return value 42 at bytes 4..8
        assert_eq!(read_i32_le(&reply, 4), Some(42));
    }

    #[test]
    fn read_i32_le_out_of_range_is_none() {
        let reply = [0u8, 0, 0, 0];
        assert_eq!(read_i32_le(&reply, 4), None);
    }
}
