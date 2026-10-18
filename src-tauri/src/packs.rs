use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

const MANIFEST_FILE: &str = "pack.json";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PackRef {
    pub pack_id: String,
    pub name: String,
    pub version: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct PackSource {
    pub kind: String,
    pub folder_id: String,
    pub resource_key: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct InstalledMod {
    pub hash: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Published {
    pub folder_id: String,
    pub dirty: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PackManifest {
    pub pack_id: String,
    pub name: String,
    pub version: Option<String>,
    pub mod_ids: Vec<String>,
    pub imported_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PackSource>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub installed_hashes: BTreeMap<String, InstalledMod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<Published>,
}

pub fn read_manifest(pack_dir: &Path) -> Option<PackManifest> {
    let path = pack_dir.join(MANIFEST_FILE);
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content)
        .map_err(|e| eprintln!("[PACKS] Could not parse {}: {}", path.display(), e))
        .ok()
}

pub fn write_manifest(pack_dir: &Path, m: &PackManifest) -> Result<(), String> {
    let json = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    let tmp = pack_dir.join(format!("{}.tmp", MANIFEST_FILE));
    fs::write(&tmp, json).map_err(|e| e.to_string())?;
    fs::rename(&tmp, pack_dir.join(MANIFEST_FILE)).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

pub fn ensure_pack_editable(mods_dir: &Path, pack_id: &str) -> Result<(), String> {
    match read_manifest(&mods_dir.join(pack_id)) {
        Some(m) if m.source.is_some() => {
            Err(format!("{} is linked to its creator. Fork it to make changes.", m.name))
        }
        _ => Ok(()),
    }
}

pub fn ensure_editable(mods_dir: &Path, key: &str) -> Result<(), String> {
    match parse_key(key) {
        Ok(KeyKind::Pack { pack_id, .. }) => ensure_pack_editable(mods_dir, &pack_id),
        _ => Ok(()),
    }
}

pub fn mark_pack_dirty(mods_dir: &Path, pack_id: &str) {
    let pack_dir = mods_dir.join(pack_id);
    let Some(mut m) = read_manifest(&pack_dir) else { return };
    let Some(published) = m.published.as_mut() else { return };
    if published.dirty {
        return;
    }
    published.dirty = true;
    if let Err(e) = write_manifest(&pack_dir, &m) {
        eprintln!("[PACKS] Could not mark {} as changed: {}", pack_id, e);
    }
}

pub fn mark_dirty(mods_dir: &Path, key: &str) {
    if let Ok(KeyKind::Pack { pack_id, .. }) = parse_key(key) {
        mark_pack_dirty(mods_dir, &pack_id);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum KeyKind {
    Loose(u32),
    Pack { pack_id: String, nexus_id: u32 },
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

pub fn parse_key(key: &str) -> Result<KeyKind, String> {
    let bad = || format!("Invalid mod key: {:?}", key);
    let parts: Vec<&str> = key.split('/').collect();
    match parts.as_slice() {
        [id] if all_digits(id) => id.parse().map(KeyKind::Loose).map_err(|_| bad()),
        [pack, id] if all_digits(id) => {
            let valid = !pack.is_empty()
                && !all_digits(pack)
                && pack.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
            if !valid {
                return Err(bad());
            }
            let nexus_id = id.parse().map_err(|_| bad())?;
            Ok(KeyKind::Pack { pack_id: pack.to_string(), nexus_id })
        }
        _ => Err(bad()),
    }
}

pub fn resolve_key(mods_dir: &Path, key: &str) -> Result<PathBuf, String> {
    Ok(match parse_key(key)? {
        KeyKind::Loose(_) => mods_dir.join(key),
        KeyKind::Pack { .. } => key.split('/').fold(mods_dir.to_path_buf(), |p, part| p.join(part)),
    })
}

pub fn resolve_for_delete(mods_dir: &Path, key: &str) -> Result<PathBuf, String> {
    let parts: Vec<&str> = key.split('/').collect();
    let ok = (1..=2).contains(&parts.len())
        && parts.iter().all(|p| !p.is_empty() && !p.starts_with('.') && !p.contains('\\') && !p.contains(':'));
    let path = parts.iter().fold(mods_dir.to_path_buf(), |p, part| p.join(part));
    if !ok || !path.is_dir() {
        return Err(format!("No mod folder for key: {:?}", key));
    }
    if looks_like_pack(&path) {
        return Err("This looks like a modpack folder. Delete it from the Modpacks list.".to_string());
    }
    Ok(path)
}

fn looks_like_pack(dir: &Path) -> bool {
    if dir.join(MANIFEST_FILE).is_file() {
        return true;
    }
    fs::read_dir(dir).map_or(false, |entries| {
        entries.filter_map(Result::ok).any(|e| e.path().is_dir() && all_digits(&e.file_name().to_string_lossy()))
    })
}

pub fn link_prefix(key: &str) -> String {
    format!("{}_", key.replace('/', "."))
}

fn is_version_token(t: &str) -> bool {
    let b = t.as_bytes();
    let mut i = usize::from(matches!(b.first(), Some(b'v' | b'V')));
    loop {
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
        } else {
            break;
        }
    }
    i == b.len() || (i + 1 == b.len() && b[i].is_ascii_alphabetic())
}

pub fn split_version(name: &str) -> (String, Option<String>) {
    let name = name.trim();
    if let Some((rest, token)) = name.rsplit_once(char::is_whitespace) {
        if is_version_token(token) {
            let version = token.strip_prefix(['v', 'V']).unwrap_or(token);
            return (rest.trim().to_string(), Some(version.to_string()));
        }
    }
    (name.to_string(), None)
}

pub fn slugify(name: &str) -> String {
    let slug = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "pack".to_string()
    } else if all_digits(&slug) {
        format!("pack-{}", slug)
    } else {
        slug
    }
}

pub fn pack_identity(preset: &serde_json::Value) -> PackRef {
    let text = |key: &str| preset.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let (name, split) = split_version(text("name").unwrap_or(""));
    match text("pack_id") {
        Some(id) => PackRef {
            pack_id: slugify(id),
            name,
            version: text("version").map(str::to_string).or(split),
        },
        None => PackRef { pack_id: slugify(&name), name, version: split },
    }
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
    fn resolve_key_joins_key_text_and_rejects_bad_keys() {
        let m = Path::new("/m");
        assert_eq!(resolve_key(m, "5543").unwrap(), Path::new("/m/5543"));
        assert_eq!(resolve_key(m, "007").unwrap(), Path::new("/m/007"));
        assert_eq!(resolve_key(m, "Casual-Shitters/3063").unwrap(), Path::new("/m/Casual-Shitters/3063"));
        for bad in ["..", "../5543", "a/b/1"] {
            assert!(resolve_key(m, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn resolve_for_delete_accepts_odd_names_but_not_traversal() {
        let m = scratch("delkey");
        fs::create_dir_all(m.join("notes")).unwrap();
        fs::create_dir_all(m.join("Foo_Bar/1")).unwrap();
        assert!(resolve_for_delete(&m, "notes").is_ok());
        assert!(resolve_for_delete(&m, "Foo_Bar/1").is_ok());
        for bad in ["..", "../x", ".import-x", "missing", "a/b/c"] {
            assert!(resolve_for_delete(&m, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parse_key_accepts_loose_and_pack_keys_only() {
        assert_eq!(parse_key("5543").unwrap(), KeyKind::Loose(5543));
        assert_eq!(parse_key("Casual-Shitters/3063").unwrap(),
            KeyKind::Pack { pack_id: "Casual-Shitters".into(), nexus_id: 3063 });
        for bad in ["..", "../5543", "a/b/1", "Casual-Shitters/x", "123/45", "", "notes", "Foo_Bar/1"] {
            assert!(parse_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pack_identity_from_names() {
        use serde_json::json;
        let id = |v: serde_json::Value| { let r = pack_identity(&v); (r.pack_id, r.name, r.version) };
        let s = |x: &str| x.to_string();
        assert_eq!(id(json!({"name": "Casual Shitters v1.5"})), (s("Casual-Shitters"), s("Casual Shitters"), Some(s("1.5"))));
        assert_eq!(id(json!({"name": "Casual Shitters 1.5"})), (s("Casual-Shitters"), s("Casual Shitters"), Some(s("1.5"))));
        assert_eq!(id(json!({"name": "Pack V2"})), (s("Pack"), s("Pack"), Some(s("2"))));
        assert_eq!(id(json!({"name": "Night Ops 2.0b"})), (s("Night-Ops"), s("Night Ops"), Some(s("2.0b"))));
        assert_eq!(id(json!({"name": "No Version Here"})), (s("No-Version-Here"), s("No Version Here"), None));
        assert_eq!(id(json!({"name": "1.5"})), (s("1-5"), s("1.5"), None));
        assert_eq!(id(json!({"name": "2024"})), (s("pack-2024"), s("2024"), None));
        assert_eq!(id(json!({"name": "!!!"})).0, "pack");
        assert_eq!(id(json!({"name": "\u{041f}\u{0430}\u{043a} v2"})).0, "pack");
        assert_eq!(id(json!({"name": "Renamed v9", "pack_id": "Casual-Shitters", "version": "1.6"})),
            (s("Casual-Shitters"), s("Renamed"), Some(s("1.6"))));
    }

    #[test]
    fn link_prefix_replaces_slash() {
        assert_eq!(link_prefix("5543"), "5543_");
        assert_eq!(link_prefix("Casual-Shitters/3063"), "Casual-Shitters.3063_");
    }

    #[test]
    fn resolve_for_delete_refuses_pack_shaped_folders() {
        let m = scratch("delpack");
        fs::create_dir_all(m.join("Broken")).unwrap();
        fs::write(m.join("Broken/pack.json"), b"{not json").unwrap();
        fs::create_dir_all(m.join("NoManifest/3063")).unwrap();
        for key in ["Broken", "NoManifest"] {
            assert_eq!(resolve_for_delete(&m, key).unwrap_err(),
                "This looks like a modpack folder. Delete it from the Modpacks list.", "{key}");
        }
        fs::create_dir_all(m.join("5543/Content")).unwrap();
        assert!(resolve_for_delete(&m, "5543").is_ok());
    }

    #[test]
    fn write_manifest_replaces_the_file_and_leaves_no_tmp() {
        let m = scratch("manifest");
        let mut man = PackManifest { pack_id: "CS".into(), name: "CS".into(), version: None, mod_ids: vec!["1".into()], imported_at: 0, ..Default::default() };
        write_manifest(&m, &man).unwrap();
        man.mod_ids.push("2".into());
        write_manifest(&m, &man).unwrap();
        assert_eq!(read_manifest(&m).unwrap().mod_ids, ["1", "2"]);
        let names: Vec<_> = fs::read_dir(&m).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, ["pack.json"]);
    }

    fn put_pack(mods: &Path, id: &str, name: &str, edit: impl FnOnce(&mut PackManifest)) {
        let dir = mods.join(id);
        fs::create_dir_all(&dir).unwrap();
        let mut man = PackManifest { pack_id: id.into(), name: name.into(), mod_ids: vec!["3063".into()], ..Default::default() };
        edit(&mut man);
        write_manifest(&dir, &man).unwrap();
    }

    #[test]
    fn old_pack_json_still_reads() {
        let m = scratch("oldpack");
        let old = r#"{"pack_id":"CS","name":"CS","version":null,"mod_ids":["1"],"imported_at":5}"#;
        fs::write(m.join("pack.json"), old).unwrap();
        let man = read_manifest(&m).unwrap();
        assert!(man.source.is_none() && man.installed_hashes.is_empty() && man.published.is_none());
        write_manifest(&m, &man).unwrap();
        let out = fs::read_to_string(m.join("pack.json")).unwrap();
        for key in ["source", "installed_hashes", "published"] {
            assert!(!out.contains(key), "{key}");
        }
    }

    #[test]
    fn linked_pack_refuses_edits() {
        let mods = scratch("linked");
        put_pack(&mods, "CS", "Casual Shitters", |m| {
            m.source = Some(PackSource { kind: "drive".into(), folder_id: "f".into(), resource_key: None });
        });
        assert_eq!(ensure_editable(&mods, "CS/3063").unwrap_err(),
            "Casual Shitters is linked to its creator. Fork it to make changes.");
        assert_eq!(ensure_pack_editable(&mods, "CS").unwrap_err(),
            "Casual Shitters is linked to its creator. Fork it to make changes.");
        assert!(ensure_editable(&mods, "5543").is_ok());
        assert!(ensure_editable(&mods, "not a key").is_ok());
    }

    #[test]
    fn mark_dirty_only_touches_published() {
        let mods = scratch("dirty");
        put_pack(&mods, "Pub", "Pub", |m| m.published = Some(Published { folder_id: "f".into(), dirty: false }));
        put_pack(&mods, "Plain", "Plain", |_| {});
        let before = fs::read(mods.join("Plain/pack.json")).unwrap();
        mark_dirty(&mods, "Plain/3063");
        mark_pack_dirty(&mods, "Plain");
        assert_eq!(fs::read(mods.join("Plain/pack.json")).unwrap(), before);
        mark_dirty(&mods, "Pub/3063");
        assert!(read_manifest(&mods.join("Pub")).unwrap().published.unwrap().dirty);
        mark_dirty(&mods, "5543");
    }
}
