//! Storage for offline analysis caches: one small JSON file per track.
//!
//! The beat and note analyses used to live in one big JSON per kind. Every
//! track switch parsed the whole file just to look up a single entry, and
//! every new analysis rewrote it — several megabytes of transient
//! allocations each time. One file per track keeps a switch down to reading
//! the current track's entry only.

use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::storage;

/// Directory (under the app data dir) holding the entry files of one kind.
fn cache_dir(kind: &str) -> PathBuf {
    storage::data_dir().join("analysis").join(kind)
}

/// Stable, filesystem-safe file name for a canonical track path. FNV-1a, so
/// the same path always maps to the same file across runs and processes.
fn entry_file_name(key: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}.json")
}

/// Deserialize a JSON file, tolerating a UTF-8 byte-order mark.
pub fn read_json_file<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let has_bom = reader
        .fill_buf()
        .map(|head| head.starts_with(&[0xEF, 0xBB, 0xBF]))
        .unwrap_or(false);
    if has_bom {
        let mut bom = [0_u8; 3];
        reader.read_exact(&mut bom).ok()?;
    }
    serde_json::from_reader(reader).ok()
}

/// Read the analysis entry of one track.
pub fn read_entry<T: DeserializeOwned>(kind: &str, key: &str) -> Option<T> {
    read_json_file(&cache_dir(kind).join(entry_file_name(key)))
}

/// Write the analysis entry of one track, replacing the old file atomically.
pub fn write_entry<T: Serialize>(kind: &str, key: &str, value: &T) {
    let dir = cache_dir(kind);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(entry_file_name(key));
    let temp = path.with_extension("json.tmp");
    let Ok(file) = fs::File::create(&temp) else {
        return;
    };
    let mut writer = BufWriter::new(file);
    if serde_json::to_writer(&mut writer, value).is_err() || writer.flush().is_err() {
        return;
    }
    drop(writer);
    let _ = fs::remove_file(&path);
    let _ = fs::rename(&temp, &path);
}

/// Keep the newest `keep` entry files once the directory holds more than
/// `max`. Every update rewrites the file, so the mtime doubles as the save
/// time.
pub fn prune_entries(kind: &str, max: usize, keep: usize) {
    let Ok(read) = fs::read_dir(cache_dir(kind)) else {
        return;
    };
    let mut files: Vec<(PathBuf, SystemTime)> = read
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map_or(false, |ext| ext == "json")
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((entry.path(), modified))
        })
        .collect();
    if files.len() <= max {
        return;
    }
    files.sort_by_key(|(_, modified)| *modified);
    let remove = files.len() - keep;
    for (path, _) in files.into_iter().take(remove) {
        let _ = fs::remove_file(path);
    }
}
