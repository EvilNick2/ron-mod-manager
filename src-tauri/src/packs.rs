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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackManifest {
    pub pack_id: String,
    pub name: String,
    pub version: Option<String>,
    pub mod_ids: Vec<String>,
    pub imported_at: u64,
}

pub fn read_manifest(pack_dir: &Path) -> Option<PackManifest> {
    let content = fs::read_to_string(pack_dir.join(MANIFEST_FILE)).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write_manifest(pack_dir: &Path, m: &PackManifest) -> Result<(), String> {
    let json = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    fs::write(pack_dir.join(MANIFEST_FILE), json).map_err(|e| e.to_string())
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
    Ok(path)
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
}
