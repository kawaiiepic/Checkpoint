//! Eglantine save directory discovery, based on savedata_factory.cpp.
//! Read-only: never creates or modifies game save directories.
use serde::Serialize;
use std::{collections::BTreeSet, env, ffi::OsStr, fs, path::{Path, PathBuf}, time::UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SaveLocation {
    pub path: String,
    pub layout: String,
    pub profile: String,
    pub file_count: usize,
    pub signature: String,
    /// Newest regular save file mtime in Unix milliseconds, if available.
    pub last_modified_ms: Option<u64>,
}

pub fn valid_title_id(id: &str) -> bool {
    id.len() == 16 && id.starts_with("0400") && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn scan_files(path: &Path) -> (usize, String, Option<u64>) {
    let mut paths = Vec::new();
    let mut latest_ms: Option<u64> = None;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if paths.len() >= 50_000 { break; }
        let Ok(entries) = fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
            if meta.file_type().is_symlink() { continue; }
            if meta.is_dir() { stack.push(entry.path()); }
            if meta.is_file() {
                let timestamp = meta.modified().ok().and_then(|t|t.duration_since(UNIX_EPOCH).ok());
                let modified = timestamp.map_or(0, |t|t.as_nanos());
                // Eglantine's bookkeeping file is not actual player progress.
                if entry.file_name() != OsStr::new(".yuzu_save_size") {
                    if let Some(ms) = timestamp.and_then(|t|u64::try_from(t.as_millis()).ok()) {
                        latest_ms = Some(latest_ms.map_or(ms, |previous| previous.max(ms)));
                    }
                }
                paths.push((entry.path(), meta.len(), modified));
            }
        }
    }
    paths.sort_by(|a,b| a.0.cmp(&b.0));
    let mut signature = String::new();
    // The signature is metadata-based and used for change detection only.
    // It is NOT a content checksum and must never be used for conflict resolution.
    for (path, size, modified) in &paths {
        use std::fmt::Write as _;
        let _ = write!(signature, "{}:{size}:{modified}\n", path.display());
    }
    (paths.len(), format!("{:016x}", stable_hash(signature.as_bytes())), latest_ms)
}
fn stable_hash(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in data { h ^= u64::from(*byte); h = h.wrapping_mul(0x100000001b3); }
    h
}

pub fn candidate_roots(extra: &[PathBuf], pids: &[u32]) -> Vec<PathBuf> {
    let home = PathBuf::from(env::var_os("HOME").unwrap_or_default());
    let xdg = env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let mut candidates: Vec<PathBuf> = extra.to_vec();
    for name in ["eglantine", "Eglantine", "eden", "Eden"] {
        candidates.push(xdg.join(name).join("nand/user/save"));
    }
    if let Some(env_roots) = env::var_os("EGLANTINE_SAVE_DIRS") {
        candidates.extend(env::split_paths(&env_roots));
    }
    for pid in pids {
        if let Ok(cwd) = fs::read_link(format!("/proc/{pid}/cwd")) {
            candidates.push(cwd.join("user/nand/user/save"));
        }
        // Portable AppImages can have their user data beside the AppImage.
        if let Ok(bytes) = fs::read(format!("/proc/{pid}/environ")) {
            for value in bytes.split(|b| *b == 0) {
                if let Some(value) = value.strip_prefix(b"APPIMAGE=") {
                    let path = PathBuf::from(String::from_utf8_lossy(value).as_ref());
                    if let Some(parent) = path.parent() {
                        candidates.push(parent.join("user/nand/user/save"));
                    }
                }
            }
        }
    }
    let mut unique = BTreeSet::new();
    for candidate in candidates {
        if let Ok(root) = fs::canonicalize(&candidate) {
            if root.is_dir() { unique.insert(root); }
        }
    }
    unique.into_iter().collect()
}
fn add_location(out: &mut Vec<SaveLocation>, path: &Path, layout: &str, profile: &str) {
    if !path.is_dir() { return; }
    let (count, signature, last_modified_ms) = scan_files(path);
    if count == 0 { return; }
    out.push(SaveLocation {
        path: path.to_string_lossy().into_owned(),
        layout: layout.into(),
        profile: profile.into(),
        file_count: count,
        signature,
        last_modified_ms,
    });
}
pub fn discover(roots: &[PathBuf], id: &str) -> Vec<SaveLocation> {
    if !valid_title_id(id) { return vec![]; }
    let id = id.to_ascii_uppercase();
    let normalized = format!("{:016X}", u64::from_str_radix(&id, 16).unwrap() & !0xff);
    let mut saves = Vec::new();
    for root in roots {
        // Current account layout: user/save/account/<UUID>/<program id without low 8 bits>/0
        let accounts = root.join("account");
        if let Ok(profiles) = fs::read_dir(accounts) {
            for profile in profiles.flatten() {
                if profile.file_type().is_ok_and(|f| f.is_dir() && !f.is_symlink()) {
                    let name = profile.file_name().to_string_lossy().into_owned();
                    add_location(&mut saves, &profile.path().join(&normalized).join("0"), "account", &name);
                }
            }
        }
        // Current device layout: user/save/device/<program id without low 8 bits>/0
        add_location(&mut saves, &root.join("device").join(&normalized).join("0"), "device", "device");
        // Legacy user layout: user/save/0000000000000000/<32-hex user id>/<full program id>
        let legacy = root.join("0000000000000000");
        if let Ok(profiles) = fs::read_dir(legacy) {
            for profile in profiles.flatten() {
                let name = profile.file_name().to_string_lossy().into_owned();
                if name.len() != 32 || !name.bytes().all(|b| b.is_ascii_hexdigit()) { continue; }
                if profile.file_type().is_ok_and(|f| f.is_dir() && !f.is_symlink()) {
                    add_location(&mut saves, &profile.path().join(&id), "legacy-account", &name);
                }
            }
        }
    }
    saves.sort_by(|a,b| a.path.cmp(&b.path));
    saves.dedup_by(|a,b| a.path == b.path);
    saves
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn switch2_only() {
        assert!(valid_title_id("04007EF01F8AE000"));
        assert!(!valid_title_id("01007EF01F8AE000"));
    }
    #[test] fn timestamps_are_optional_for_missing_data() {
        let root = std::env::temp_dir().join(format!("checkpoint-empty-test-{}",std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(scan_files(&root).2, None);
        let _ = std::fs::remove_dir(&root);
    }
    #[test] fn stable_hash_changes() { assert_ne!(stable_hash(b"a"), stable_hash(b"b")); }
}
