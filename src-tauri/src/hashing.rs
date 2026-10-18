#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::packs::CachedHash;

pub const META_FILES: [&str; 2] = ["metadata.json", "thumbnail.jpg"];

const CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("Could not open {}: {}", path.display(), e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("Could not read {}: {}", path.display(), e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn list_mod_files(mod_dir: &Path) -> Result<Vec<(String, PathBuf, u64)>, String> {
    let mut files = Vec::new();
    for entry in WalkDir::new(mod_dir).follow_links(false) {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(mod_dir)
            .map_err(|e| e.to_string())?;
        let rel_path = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let size = entry.metadata().map_err(|e| e.to_string())?.len();
        files.push((rel_path, entry.path().to_path_buf(), size));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

pub fn hash_mod_files(
    mod_dir: &Path,
    on_file: &mut dyn FnMut(&str),
) -> Result<Vec<FileEntry>, String> {
    let mut entries = Vec::new();
    for (path, full_path, size) in list_mod_files(mod_dir)? {
        let sha256 = sha256_file(&full_path)?;
        on_file(&path);
        entries.push(FileEntry { path, size, sha256 });
    }
    Ok(entries)
}

pub fn mtime_ns(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    to_ns(modified)
}

fn to_ns(time: std::time::SystemTime) -> Option<u64> {
    let since = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(since.as_nanos()).ok()
}

fn stat(path: &Path) -> (Option<u64>, Option<u64>) {
    match fs::metadata(path) {
        Ok(m) => (Some(m.len()), m.modified().ok().and_then(to_ns)),
        Err(_) => (None, None),
    }
}

// Timestamps move in coarse ticks, so a file this new could still change without its mtime moving
const RACY_NS: u64 = 2_000_000_000;

// The mtime a freshly hashed file is cached under, or None when it must be hashed again next time:
// mtime unreadable, size or mtime changed while it was read, or mtime too close to now
fn cache_mtime(before: (u64, Option<u64>), after: (Option<u64>, Option<u64>), now_ns: u64) -> Option<u64> {
    let mtime = before.1?;
    if after != (Some(before.0), Some(mtime)) || now_ns.saturating_sub(mtime) < RACY_NS {
        return None;
    }
    Some(mtime)
}

// Like hash_mod_files, but a file whose size and mtime match its cache entry keeps the cached
// sha256 without being read. Files safe to trust next time are recorded in fresh.
pub fn hash_mod_files_cached(
    mod_dir: &Path,
    mod_id: &str,
    cache: &BTreeMap<String, CachedHash>,
    fresh: &mut BTreeMap<String, CachedHash>,
    on_file: &mut dyn FnMut(&str),
) -> Result<Vec<FileEntry>, String> {
    let mut entries = Vec::new();
    for (path, full_path, size) in list_mod_files(mod_dir)? {
        let key = format!("{}/{}", mod_id, path);
        let mtime = mtime_ns(&full_path);
        let hit = cache
            .get(&key)
            .filter(|c| mtime.is_some_and(|m| c.size == size && c.mtime_ns == m));
        let sha256 = match hit {
            Some(c) => {
                fresh.insert(key, c.clone());
                c.sha256.clone()
            }
            None => {
                let sha256 = sha256_file(&full_path)?;
                let now = to_ns(std::time::SystemTime::now()).unwrap_or(0);
                if let Some(mtime_ns) = cache_mtime((size, mtime), stat(&full_path), now) {
                    fresh.insert(key, CachedHash { size, mtime_ns, sha256: sha256.clone() });
                }
                sha256
            }
        };
        on_file(&path);
        entries.push(FileEntry { path, size, sha256 });
    }
    Ok(entries)
}

fn read_metadata(mod_dir: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(mod_dir.join("metadata.json")).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn addon_states(mod_dir: &Path) -> BTreeMap<String, bool> {
    let mut states = BTreeMap::new();
    let Ok(entries) = fs::read_dir(mod_dir.join("addons")) else {
        return states;
    };
    let metadata = read_metadata(mod_dir);
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let is_pak = path.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("pak"));
        if !is_pak {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let enabled = metadata
            .as_ref()
            .and_then(|m| m.get("addons"))
            .and_then(|a| a.get(&name))
            .and_then(|a| a.get("enabled"))
            .and_then(|e| e.as_bool())
            .unwrap_or(false);
        states.insert(name, enabled);
    }
    states
}

pub fn set_addon_states(mod_dir: &Path, states: &BTreeMap<String, bool>) -> Result<(), String> {
    let path = mod_dir.join("metadata.json");
    let mut root = match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| format!("Could not parse {}: {}", path.display(), e))?,
        Err(_) => serde_json::json!({}),
    };
    let not_object = || format!("{} is not a JSON object", path.display());
    let root_obj = root.as_object_mut().ok_or_else(not_object)?;
    let addons = root_obj
        .entry("addons")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(not_object)?;
    for (name, enabled) in states {
        addons.insert(name.clone(), serde_json::json!({ "enabled": enabled }));
    }
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| format!("Could not write {}: {}", path.display(), e))
}

pub fn mod_hash<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
    addons: &BTreeMap<String, bool>,
) -> String {
    let mut files: Vec<(&str, &str)> = files
        .into_iter()
        .filter(|(path, _)| !is_meta_file(path))
        .collect();
    files.sort_by(|a, b| a.0.cmp(b.0));
    let mut text = String::new();
    for (path, sha256) in files {
        text.push_str(&format!("{}\t{}\n", path, sha256));
    }
    for (name, enabled) in addons {
        let state = if *enabled { "on" } else { "off" };
        text.push_str(&format!("addon\t{}\t{}\n", name, state));
    }
    sha256_bytes(text.as_bytes())
}

pub fn is_meta_file(path: &str) -> bool {
    META_FILES.contains(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ron-test-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sha256_known_values() {
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        let dir = scratch("sha256-file");
        let path = dir.join("abc.txt");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn mod_hash_exact_format() {
        let file_hash = sha256_bytes(b"abc");
        let addons = BTreeMap::from([("x.pak".to_string(), true)]);
        let hash = mod_hash([("a.pak", file_hash.as_str())], &addons);
        assert_eq!(
            hash,
            "167c886dc2d3daa3c2a1335660108fc6707b6602b45f0c34a0d937e77647bc02"
        );
    }

    #[test]
    fn mod_hash_ignores_meta_and_order() {
        let a = sha256_bytes(b"abc");
        let b = sha256_bytes(b"def");
        let m = sha256_bytes(b"meta");
        let t = sha256_bytes(b"thumb");
        let addons = BTreeMap::new();

        let plain = mod_hash([("a.pak", a.as_str()), ("b.pak", b.as_str())], &addons);
        let reversed = mod_hash([("b.pak", b.as_str()), ("a.pak", a.as_str())], &addons);
        let with_meta = mod_hash(
            [
                ("metadata.json", m.as_str()),
                ("a.pak", a.as_str()),
                ("thumbnail.jpg", t.as_str()),
                ("b.pak", b.as_str()),
            ],
            &addons,
        );
        assert_eq!(plain, reversed);
        assert_eq!(plain, with_meta);

        let nested_meta = mod_hash(
            [("a.pak", a.as_str()), ("b.pak", b.as_str()), ("addons/metadata.json", m.as_str())],
            &addons,
        );
        assert_ne!(plain, nested_meta);
    }

    #[test]
    fn mod_hash_sees_addon_toggle() {
        let a = sha256_bytes(b"abc");
        let on = BTreeMap::from([("x.pak".to_string(), true)]);
        let off = BTreeMap::from([("x.pak".to_string(), false)]);
        assert_ne!(
            mod_hash([("a.pak", a.as_str())], &on),
            mod_hash([("a.pak", a.as_str())], &off)
        );
    }

    #[test]
    fn list_mod_files_nested() {
        let dir = scratch("list-files");
        fs::write(dir.join("a.pak"), b"one").unwrap();
        fs::create_dir_all(dir.join("addons")).unwrap();
        fs::write(dir.join("addons").join("b.pak"), b"four").unwrap();

        let listed: Vec<(String, u64)> = list_mod_files(&dir)
            .unwrap()
            .into_iter()
            .map(|(path, _, size)| (path, size))
            .collect();
        assert_eq!(
            listed,
            vec![("a.pak".to_string(), 3), ("addons/b.pak".to_string(), 4)]
        );
    }

    #[test]
    fn addon_states_round_trip() {
        let dir = scratch("addon-states");
        fs::create_dir_all(dir.join("addons")).unwrap();
        fs::write(dir.join("addons").join("x.pak"), b"x").unwrap();
        fs::write(dir.join("addons").join("y.pak"), b"y").unwrap();
        fs::write(dir.join("addons").join("z.PAK"), b"z").unwrap();
        fs::write(dir.join("addons").join("notes.txt"), b"n").unwrap();
        fs::write(dir.join("metadata.json"), br#"{"name": "Mod"}"#).unwrap();

        let states = BTreeMap::from([
            ("x.pak".to_string(), true),
            ("y.pak".to_string(), false),
        ]);
        set_addon_states(&dir, &states).unwrap();

        let read = addon_states(&dir);
        assert_eq!(
            read,
            BTreeMap::from([
                ("x.pak".to_string(), true),
                ("y.pak".to_string(), false),
                ("z.PAK".to_string(), false),
            ])
        );

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(meta["name"], "Mod");
    }

    #[test]
    fn cache_mtime_rules() {
        let old = 1_000_000_000_000;
        let now = old + 60_000_000_000;
        assert_eq!(cache_mtime((3, Some(old)), (Some(3), Some(old)), now), Some(old));
        assert_eq!(cache_mtime((3, None), (Some(3), None), now), None);
        assert_eq!(cache_mtime((3, Some(old)), (Some(4), Some(old)), now), None);
        assert_eq!(cache_mtime((3, Some(old)), (Some(3), Some(old + 1)), now), None);
        assert_eq!(cache_mtime((3, Some(old)), (None, None), now), None);
        assert_eq!(cache_mtime((3, Some(now - 1_999_999_999)), (Some(3), Some(now - 1_999_999_999)), now), None);
        assert_eq!(cache_mtime((3, Some(now + 5)), (Some(3), Some(now + 5)), now), None);
    }

    #[test]
    fn recent_mtime_is_not_cached() {
        let dir = scratch("cache-recent");
        fs::write(dir.join("new.pak"), b"new").unwrap();
        fs::write(dir.join("old.pak"), b"old").unwrap();
        let now = std::time::SystemTime::now();
        let set = |name: &str, t| File::options().write(true).open(dir.join(name)).unwrap().set_modified(t).unwrap();
        set("new.pak", now);
        set("old.pak", now - std::time::Duration::from_secs(60));

        let mut fresh = BTreeMap::new();
        let files = hash_mod_files_cached(&dir, "1", &BTreeMap::new(), &mut fresh, &mut |_| {}).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(fresh.keys().collect::<Vec<_>>(), ["1/old.pak"]);
    }
}
