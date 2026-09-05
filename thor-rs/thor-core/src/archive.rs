//! Firmware archive handling: Odin `.tar` / `.tar.md5` packages and LZ4-compressed
//! partition images.
//!
//! Odin firmware ships as tar archives (sometimes with an MD5 checksum appended, hence
//! `.tar.md5`) whose top-level files are partition images, some LZ4-compressed (`.lz4`).
//! Flashing needs the *decompressed* size up front (to plan the sequence), which we read
//! cheaply from the LZ4 frame header rather than by decompressing.

use std::io::Read;

/// LZ4 frame magic, little-endian on disk (`0x184D2204`).
const LZ4_FRAME_MAGIC: [u8; 4] = [0x04, 0x22, 0x4D, 0x18];

/// Something went wrong reading an archive or compressed image.
#[derive(Debug)]
pub enum ArchiveError {
    Lz4(String),
    Tar(String),
    NotFound(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::Lz4(m) => write!(f, "LZ4 error: {m}"),
            ArchiveError::Tar(m) => write!(f, "tar error: {m}"),
            ArchiveError::NotFound(n) => write!(f, "entry not found in archive: {n}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

/// A top-level file inside a firmware tar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarEntryInfo {
    pub name: String,
    /// Size of the entry as stored in the tar (i.e. still LZ4-compressed if `.lz4`).
    pub size: u64,
}

/// The uncompressed size declared in an LZ4 frame header, if it carries one (Odin's do).
/// Returns `None` if the data isn't an LZ4 frame or omits the content size.
pub fn lz4_content_size(data: &[u8]) -> Option<u64> {
    if !data.starts_with(&LZ4_FRAME_MAGIC) {
        return None;
    }
    // FLG byte at offset 4; the "content size present" flag is bit 3 (0x08). When set, an
    // 8-byte little-endian content size follows the FLG and BD bytes (offset 6..14).
    let flg = *data.get(4)?;
    if flg & 0x08 == 0 {
        return None;
    }
    let bytes: [u8; 8] = data.get(6..14)?.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

/// Wrap a reader in a streaming LZ4-frame decompressor: reads compressed bytes from
/// `reader`, yields decompressed bytes. Lets a multi-GB image be flashed without ever
/// holding it all in memory. The caller learns the decompressed length separately from
/// [`lz4_content_size`].
pub fn lz4_stream_reader<R: Read>(reader: R) -> impl Read {
    lz4_flex::frame::FrameDecoder::new(reader)
}

/// Fully decompress an LZ4 frame into memory.
pub fn decompress_lz4(data: &[u8]) -> Result<Vec<u8>, ArchiveError> {
    let mut out = Vec::new();
    lz4_flex::frame::FrameDecoder::new(data)
        .read_to_end(&mut out)
        .map_err(|e| ArchiveError::Lz4(e.to_string()))?;
    Ok(out)
}

/// List the top-level file entries of a tar (or `.tar.md5`) archive. Generic over the
/// reader so callers can stream from a `File` instead of buffering the whole archive.
pub fn list_tar<R: Read>(reader: R) -> Result<Vec<TarEntryInfo>, ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|e| ArchiveError::Tar(e.to_string()))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| ArchiveError::Tar(e.to_string()))?;
        let path = entry.path().map_err(|e| ArchiveError::Tar(e.to_string()))?;
        // Only top-level files (no directory component) — matches the C#'s behavior.
        if path.components().count() == 1 {
            out.push(TarEntryInfo {
                name: path.to_string_lossy().into_owned(),
                size: entry.size(),
            });
        }
    }
    Ok(out)
}

/// Extract one named top-level entry's bytes from a tar archive.
pub fn extract_tar<R: Read>(reader: R, name: &str) -> Result<Vec<u8>, ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|e| ArchiveError::Tar(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| ArchiveError::Tar(e.to_string()))?;
        let matches = entry
            .path()
            .map_err(|e| ArchiveError::Tar(e.to_string()))?
            .to_string_lossy()
            == name;
        if matches {
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry
                .read_to_end(&mut buf)
                .map_err(|e| ArchiveError::Tar(e.to_string()))?;
            return Ok(buf);
        }
    }
    Err(ArchiveError::NotFound(name.to_string()))
}

/// One image inside a firmware archive, with its real (on-device) size resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveImage {
    /// Tar entry name, e.g. `boot.img.lz4`.
    pub name: String,
    /// Size as stored in the tar (compressed, for `.lz4`).
    pub stored_size: u64,
    /// Size once written to the device (LZ4-decompressed size for `.lz4`, else `stored_size`).
    pub real_size: u64,
    /// Whether the entry is LZ4-compressed.
    pub compressed: bool,
}

/// List every top-level image in an archive with its real on-device size. For `.lz4`
/// entries the decompressed size is read from the frame header (only ~16 bytes per entry),
/// so this stays cheap even for multi-GB firmware.
pub fn list_archive_images<R: Read>(reader: R) -> Result<Vec<ArchiveImage>, ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|e| ArchiveError::Tar(e.to_string()))?;
    let mut out = Vec::new();
    for entry in entries {
        let mut entry = entry.map_err(|e| ArchiveError::Tar(e.to_string()))?;
        let path = entry.path().map_err(|e| ArchiveError::Tar(e.to_string()))?;
        if path.components().count() != 1 {
            continue; // top-level files only
        }
        let name = path.to_string_lossy().into_owned();
        let stored_size = entry.size();
        let compressed = name.ends_with(".lz4");
        let real_size = if compressed {
            // Read just the frame header to learn the decompressed size, then move on.
            let mut hdr = [0u8; 16];
            let n = read_up_to(&mut entry, &mut hdr)?;
            lz4_content_size(&hdr[..n]).unwrap_or(stored_size)
        } else {
            stored_size
        };
        out.push(ArchiveImage {
            name,
            stored_size,
            real_size,
            compressed,
        });
    }
    Ok(out)
}

fn read_up_to(reader: &mut dyn Read, buf: &mut [u8]) -> Result<usize, ArchiveError> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(ArchiveError::Tar(e.to_string())),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lz4_flex::frame::{FrameEncoder, FrameInfo};
    use std::io::Write;
    use tar::{Builder, Header};

    fn lz4_compress_with_size(payload: &[u8]) -> Vec<u8> {
        let info = FrameInfo::new().content_size(Some(payload.len() as u64));
        let mut enc = FrameEncoder::with_frame_info(info, Vec::new());
        enc.write_all(payload).unwrap();
        enc.finish().unwrap()
    }

    fn build_tar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = Builder::new(Vec::new());
        for (name, data) in files {
            let mut h = Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn lz4_content_size_reads_the_header() {
        let payload = b"partition image contents ".repeat(40);
        let compressed = lz4_compress_with_size(&payload);
        assert_eq!(lz4_content_size(&compressed), Some(payload.len() as u64));
    }

    #[test]
    fn lz4_content_size_none_for_non_lz4() {
        assert_eq!(lz4_content_size(b"not an lz4 frame at all"), None);
    }

    #[test]
    fn decompress_lz4_round_trips() {
        let payload = b"the quick brown boot.img ".repeat(100);
        let compressed = lz4_compress_with_size(&payload);
        assert_eq!(decompress_lz4(&compressed).unwrap(), payload);
    }

    #[test]
    fn lz4_stream_reader_round_trips() {
        let payload = b"streamed partition image ".repeat(200);
        let compressed = lz4_compress_with_size(&payload);
        let mut out = Vec::new();
        lz4_stream_reader(&compressed[..])
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn list_tar_returns_top_level_files_with_sizes() {
        let tar = build_tar(&[("boot.img", b"BOOTDATA"), ("recovery.img", b"RECOVERY!!")]);
        let entries = list_tar(&tar[..]).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&TarEntryInfo {
            name: "boot.img".into(),
            size: 8
        }));
        assert!(entries.contains(&TarEntryInfo {
            name: "recovery.img".into(),
            size: 10
        }));
    }

    #[test]
    fn extract_tar_returns_entry_bytes() {
        let tar = build_tar(&[("boot.img", b"BOOTDATA"), ("recovery.img", b"RECOVERY!!")]);
        assert_eq!(extract_tar(&tar[..], "boot.img").unwrap(), b"BOOTDATA");
    }

    #[test]
    fn extract_tar_missing_is_not_found() {
        let tar = build_tar(&[("boot.img", b"BOOTDATA")]);
        assert!(matches!(
            extract_tar(&tar[..], "nope.img"),
            Err(ArchiveError::NotFound(_))
        ));
    }

    #[test]
    fn list_archive_images_resolves_lz4_real_sizes() {
        let payload = b"boot partition payload ".repeat(64); // 1472 bytes
        let compressed = lz4_compress_with_size(&payload);
        let tar = build_tar(&[
            ("boot.img.lz4", &compressed),
            ("dtbo.img", b"DTBO-RAW-DATA"),
        ]);

        let images = list_archive_images(&tar[..]).unwrap();
        assert_eq!(images.len(), 2);

        let boot = images.iter().find(|i| i.name == "boot.img.lz4").unwrap();
        assert!(boot.compressed);
        assert_eq!(boot.stored_size, compressed.len() as u64);
        assert_eq!(boot.real_size, payload.len() as u64); // decompressed size from header

        let dtbo = images.iter().find(|i| i.name == "dtbo.img").unwrap();
        assert!(!dtbo.compressed);
        assert_eq!(dtbo.real_size, dtbo.stored_size);
        assert_eq!(dtbo.real_size, 13);
    }
}
