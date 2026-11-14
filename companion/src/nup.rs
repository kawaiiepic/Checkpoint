//! Read-only NUP v1 metadata inspector. Never reads game extents.
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NupInfo {
    pub title_id: String,
    pub title: Option<String>,
    pub base_game: bool,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn u16_at(data: &[u8], pos: usize) -> io::Result<u16> {
    Ok(u16::from_le_bytes(data.get(pos..pos + 2).ok_or_else(|| invalid("truncated field"))?.try_into().unwrap()))
}
fn u32_at(data: &[u8], pos: usize) -> io::Result<u32> {
    Ok(u32::from_le_bytes(data.get(pos..pos + 4).ok_or_else(|| invalid("truncated field"))?.try_into().unwrap()))
}
fn u64_at(data: &[u8], pos: usize) -> io::Result<u64> {
    Ok(u64::from_le_bytes(data.get(pos..pos + 8).ok_or_else(|| invalid("truncated field"))?.try_into().unwrap()))
}

pub fn inspect(path: &Path) -> io::Result<Option<NupInfo>> {
    let mut file = File::open(path)?;
    let actual_size = file.metadata()?.len();
    let mut header = [0u8; 0x80];
    file.read_exact(&mut header)?;
    if &header[0..4] != b"NUP\x01" { return Err(invalid("not NUP v1")); }
    let header_size = u16_at(&header, 4)? as u64;
    let toc_offset = u64_at(&header, 8)?;
    let toc_size = u64_at(&header, 0x10)?;
    let blob_offset = u64_at(&header, 0x38)?;
    let declared_size = u64_at(&header, 0x40)?;
    let package_count = u32_at(&header, 0x48)? as usize;
    if header_size < 0x80 || toc_offset != header_size || package_count == 0 ||
       package_count > 4096 || toc_size < 0x30 || toc_size > 64 * 1024 * 1024 ||
       toc_offset.checked_add(toc_size).is_none_or(|end| end > blob_offset || end > actual_size) ||
       declared_size != actual_size {
        return Err(invalid("invalid NUP header"));
    }
    file.seek(SeekFrom::Start(toc_offset))?;
    let mut toc = vec![0u8; toc_size as usize];
    file.read_exact(&mut toc)?;
    if Sha256::digest(&toc).as_slice() != &header[0x18..0x38] {
        return Err(invalid("NUP TOC checksum mismatch"));
    }
    if &toc[0..4] != b"TOC\x01" || u32_at(&toc, 4)? as usize != package_count {
        return Err(invalid("invalid NUP table"));
    }
    let records_size = package_count.checked_mul(0x40).and_then(|n| n.checked_add(0x30))
        .ok_or_else(|| invalid("package count overflow"))?;
    if records_size > toc.len() { return Err(invalid("truncated NUP package records")); }

    let mut selected: Option<(u8, NupInfo)> = None;
    for index in 0..package_count {
        let off = 0x30 + index * 0x40;
        let kind = toc[off + 0x18];
        if kind > 2 { continue; } // Ignore reserved/system packages.
        let title_id = u64_at(&toc, off)?;
        if title_id == 0 { continue; }
        let meta_offset = u64_at(&toc, off + 0x20)?;
        let meta_size = u32_at(&toc, off + 0x28)? as u64;
        let title = if meta_size == 0 { None } else {
            if meta_size > 4 * 1024 * 1024 ||
               meta_offset.checked_add(meta_size).is_none_or(|end| end > actual_size) {
                return Err(invalid("invalid NUP metadata bounds"));
            }
            file.seek(SeekFrom::Start(meta_offset))?;
            let mut metadata = vec![0u8; meta_size as usize];
            file.read_exact(&mut metadata)?;
            let mut pos = 0usize;
            let mut found = None;
            while pos + 8 <= metadata.len() {
                let kind = u16_at(&metadata, pos)?;
                let len = u32_at(&metadata, pos + 4)? as usize;
                pos += 8;
                let end = pos.checked_add(len).filter(|end| *end <= metadata.len())
                    .ok_or_else(|| invalid("NUP TLV out of bounds"))?;
                if kind == 5 {
                    let candidate = std::str::from_utf8(&metadata[pos..end])
                        .map_err(|_| invalid("invalid NUP title UTF-8"))?.trim_matches('\0').trim();
                    if !candidate.is_empty() { found = Some(candidate.to_owned()); }
                }
                pos = end;
            }
            found
        };
        let info = NupInfo {title_id: format!("{title_id:016X}"), title, base_game:kind==0};
        // Prefer a base game, then an update; do not select DLC.
        if kind == 0 { return Ok(Some(info)); }
        if kind == 1 && selected.is_none() { selected = Some((kind, info)); }
    }
    Ok(selected.map(|(_, info)| info))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_magic() {
        let path = std::env::temp_dir().join(format!("checkpoint-test-nup-{}", std::process::id()));
        std::fs::write(&path, [0u8; 128]).unwrap();
        assert!(inspect(&path).is_err());
        let _ = std::fs::remove_file(path);
    }
}
