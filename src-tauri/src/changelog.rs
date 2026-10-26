use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::packs::ModState;
use crate::server::VersionEntry;

const MAX_NOTES: usize = 32000;
const MAX_HISTORY: usize = 99;
const MAX_HISTORY_BYTES: usize = 1000000;
const MORE: &str = "- ...and more";

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Added { name: String, version: Option<String> },
    Removed { name: String, version: Option<String> },
    Updated { mod_id: String, name: String, from: Option<String>, to: Option<String>, nexus_mod_id: Option<u32> },
    FilesChanged { name: String },
}

pub fn diff(old: &BTreeMap<String, ModState>, new: &BTreeMap<String, ModState>) -> Vec<Change> {
    let mut changes = Vec::new();
    for (id, now) in new {
        match old.get(id) {
            None => changes.push(Change::Added { name: now.name.clone(), version: now.version.clone() }),
            Some(was) if was.version != now.version => changes.push(Change::Updated {
                mod_id: id.clone(),
                name: now.name.clone(),
                from: was.version.clone(),
                to: now.version.clone(),
                nexus_mod_id: now.nexus_mod_id.or(was.nexus_mod_id),
            }),
            Some(was) if was.hash != now.hash => changes.push(Change::FilesChanged { name: now.name.clone() }),
            Some(_) => {}
        }
    }
    for (id, was) in old {
        if !new.contains_key(id) {
            changes.push(Change::Removed { name: was.name.clone(), version: was.version.clone() });
        }
    }
    changes
}

pub fn compare_versions(a: &str, b: &str) -> Ordering {
    fn parts(v: &str) -> Vec<u64> {
        let v = v.trim();
        let v = v.strip_prefix(['v', 'V']).unwrap_or(v);
        v.split('-').next().unwrap_or("").split('.').map(js_parse_int).collect()
    }
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let ord = pa.get(i).unwrap_or(&0).cmp(pb.get(i).unwrap_or(&0));
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

// Number.parseInt(part, 10) || 0: leading whitespace, then leading digits
fn js_parse_int(part: &str) -> u64 {
    let digits: String = part.trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}

fn split_breaks(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut lines = Vec::new();
    let (mut start, mut i) = (0, 0);
    while let Some(off) = lower[i..].find("<br") {
        let at = i + off;
        let rest = &lower[at + 3..];
        let trimmed = rest.trim_start_matches(' ');
        let after = trimmed.strip_prefix('/').unwrap_or(trimmed);
        if let Some(close) = after.strip_prefix('>') {
            lines.push(text[start..at].to_string());
            start = lower.len() - close.len();
            i = start;
        } else {
            i = at + 3;
        }
    }
    lines.push(text[start..].to_string());
    lines
}

pub fn nexus_lines(changelogs: &serde_json::Value, from: Option<&str>, to: Option<&str>) -> Vec<String> {
    let (Some(map), Some(to)) = (changelogs.as_object(), to) else { return Vec::new() };
    let mut keys: Vec<&String> = map
        .keys()
        .filter(|k| match from {
            Some(from) => compare_versions(k, from) == Ordering::Greater && compare_versions(k, to) != Ordering::Greater,
            None => compare_versions(k, to) == Ordering::Equal,
        })
        .collect();
    keys.sort_by(|a, b| compare_versions(a, b));
    let mut out: Vec<String> = Vec::new();
    for key in keys {
        let Some(items) = map[key].as_array() else { continue };
        for item in items.iter().filter_map(|i| i.as_str()) {
            for line in split_breaks(item) {
                let line = line.trim();
                let line = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).unwrap_or(line).trim();
                if !line.is_empty() && !out.iter().any(|l| l == line) {
                    out.push(line.to_string());
                }
            }
        }
    }
    out
}

fn with_version(name: &str, version: &Option<String>) -> String {
    match version {
        Some(v) if !v.is_empty() => format!("{} {}", name, v),
        _ => name.to_string(),
    }
}

fn section(out: &mut String, title: &str, mut items: Vec<(String, String)>) {
    if items.is_empty() {
        return;
    }
    items.sort_by_key(|(name, _)| name.to_lowercase());
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&format!("### {}\n", title));
    for (_, text) in items {
        out.push_str(&text);
        out.push('\n');
    }
}

pub fn render(changes: &[Change], notes: &BTreeMap<String, Vec<String>>, forked_from: Option<(&str, Option<&str>)>) -> String {
    let mut out = String::new();
    if let Some((name, version)) = forked_from {
        out.push_str(&format!("Forked from {}.\n", with_version(name, &version.map(str::to_string))));
    }
    let (mut updated, mut added, mut removed, mut files) = (vec![], vec![], vec![], vec![]);
    for change in changes {
        match change {
            Change::Updated { mod_id, name, from, to, .. } => {
                let mut text = format!("- {}", name);
                match (from.as_deref().filter(|v| !v.is_empty()), to.as_deref().filter(|v| !v.is_empty())) {
                    (Some(f), Some(t)) => text.push_str(&format!(" {} to {}", f, t)),
                    (None, Some(t)) => text.push_str(&format!(" to {}", t)),
                    _ => {}
                }
                for line in notes.get(mod_id).into_iter().flatten() {
                    text.push_str(&format!("\n  - {}", line));
                }
                updated.push((name.clone(), text));
            }
            Change::Added { name, version } => added.push((name.clone(), format!("- {}", with_version(name, version)))),
            Change::Removed { name, version } => removed.push((name.clone(), format!("- {}", with_version(name, version)))),
            Change::FilesChanged { name } => files.push((name.clone(), format!("- {}", name))),
        }
    }
    section(&mut out, "Updated", updated);
    section(&mut out, "Added", added);
    section(&mut out, "Removed", removed);
    section(&mut out, "Changed files", files);
    if changes.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("No mod changes.");
    }
    out.trim_end().to_string()
}

fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

pub fn fit(notes: String) -> String {
    if units(&notes) <= MAX_NOTES {
        return notes;
    }
    let budget = MAX_NOTES - units(MORE) - 1;
    let mut out = String::new();
    for line in notes.lines() {
        let next = units(&out) + units(line) + usize::from(!out.is_empty());
        if next > budget {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(MORE);
    out
}

/// Entries run newest first
pub fn fit_history(mut entries: Vec<VersionEntry>) -> Vec<VersionEntry> {
    entries.truncate(MAX_HISTORY);
    while !entries.is_empty() && serde_json::to_vec(&entries).map(|v| v.len()).unwrap_or(usize::MAX) > MAX_HISTORY_BYTES {
        entries.pop();
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state(name: &str, version: Option<&str>, hash: &str) -> ModState {
        ModState { name: name.into(), version: version.map(str::to_string), hash: hash.into(), nexus_mod_id: None }
    }

    fn mods(items: &[(&str, ModState)]) -> BTreeMap<String, ModState> {
        items.iter().map(|(id, s)| (id.to_string(), s.clone())).collect()
    }

    #[test]
    fn diff_finds_each_kind() {
        let old = mods(&[
            ("1", state("Alpha", Some("1.0"), "a")),
            ("2", state("Bravo", Some("1.0"), "b")),
            ("3", state("Charlie", Some("2.0"), "c")),
            ("4", state("Delta", Some("1.0"), "d")),
        ]);
        let new = mods(&[
            ("1", state("Alpha", Some("1.1"), "a2")),
            ("2", state("Bravo", Some("1.0"), "b2")),
            ("3", state("Charlie", Some("2.0"), "c")),
            ("5", state("Echo", None, "e")),
        ]);
        let got = diff(&old, &new);
        assert_eq!(got.len(), 4);
        assert!(got.contains(&Change::Updated {
            mod_id: "1".into(), name: "Alpha".into(), from: Some("1.0".into()), to: Some("1.1".into()), nexus_mod_id: None,
        }));
        assert!(got.contains(&Change::FilesChanged { name: "Bravo".into() }));
        assert!(got.contains(&Change::Added { name: "Echo".into(), version: None }));
        assert!(got.contains(&Change::Removed { name: "Delta".into(), version: Some("1.0".into()) }));
    }

    #[test]
    fn diff_matches_by_mod_id() {
        let old = mods(&[("1", state("Old Name", Some("1.0"), "a"))]);
        let new = mods(&[("1", state("New Name", Some("1.0"), "a"))]);
        assert!(diff(&old, &new).is_empty());
    }

    #[test]
    fn compare_versions_matches_update_modal() {
        assert_eq!(compare_versions("1.10", "1.9"), Ordering::Greater);
        assert_eq!(compare_versions("v2", "2.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.4-beta", "1.2.4"), Ordering::Equal);
        assert_eq!(compare_versions("1.2", "1.3"), Ordering::Less);
        assert_eq!(compare_versions(" V1.0.1", "1"), Ordering::Greater);
    }

    #[test]
    fn nexus_lines_takes_the_range() {
        let log = json!({
            "1.1.0": ["- Fixed floating blood when switching weapons"],
            "1.1.2": ["Added 1 click mod installer"],
            "1.2.4": ["Minor adjustments"]
        });
        assert_eq!(
            nexus_lines(&log, Some("1.1.0"), Some("1.2.4")),
            ["Added 1 click mod installer", "Minor adjustments"]
        );
        assert_eq!(nexus_lines(&log, None, Some("1.1.0")), ["Fixed floating blood when switching weapons"]);
        assert!(nexus_lines(&log, Some("1.1.0"), None).is_empty());
    }

    #[test]
    fn nexus_lines_cleans_html_and_duplicates() {
        let log = json!({"1.1": ["Removed light function material<br />Light radius was slightly widened"]});
        assert_eq!(
            nexus_lines(&log, Some("1.0"), Some("1.1")),
            ["Removed light function material", "Light radius was slightly widened"]
        );
        let log = json!({"1.3": ["Overhauled the visual quality", "Overhauled the visual quality"]});
        assert_eq!(nexus_lines(&log, Some("1.0"), Some("1.3")), ["Overhauled the visual quality"]);
        let log = json!({"2": ["* One<BR>- Two<br/>  <br>Three"]});
        assert_eq!(nexus_lines(&log, Some("1"), Some("2")), ["One", "Two", "Three"]);
    }

    #[test]
    fn nexus_lines_empty_object_gives_nothing() {
        assert!(nexus_lines(&json!({}), Some("1.0"), Some("2.0")).is_empty());
        assert!(nexus_lines(&json!([1]), Some("1.0"), Some("2.0")).is_empty());
    }

    #[test]
    fn render_orders_sections() {
        let changes = vec![
            Change::FilesChanged { name: "Zed".into() },
            Change::Removed { name: "Gone".into(), version: Some("1.0".into()) },
            Change::Added { name: "beta".into(), version: Some("2.0".into()) },
            Change::Added { name: "Alpha".into(), version: None },
            Change::Updated { mod_id: "7".into(), name: "Mod".into(), from: Some("1.0".into()), to: Some("1.1".into()), nexus_mod_id: Some(7) },
        ];
        let notes: BTreeMap<String, Vec<String>> = [("7".to_string(), vec!["Fixed a thing".to_string()])].into();
        let got = render(&changes, &notes, Some(("Casual", Some("1.2"))));
        let want = "Forked from Casual 1.2.\n\n### Updated\n- Mod 1.0 to 1.1\n  - Fixed a thing\n\n### Added\n- Alpha\n- beta 2.0\n\n### Removed\n- Gone 1.0\n\n### Changed files\n- Zed";
        assert_eq!(got, want);
    }

    #[test]
    fn render_no_changes() {
        assert_eq!(render(&[], &BTreeMap::new(), None), "No mod changes.");
        assert_eq!(render(&[], &BTreeMap::new(), Some(("Casual", Some("1.2")))), "Forked from Casual 1.2.\n\nNo mod changes.");
    }

    #[test]
    fn render_updated_with_a_missing_side() {
        let up = |from: Option<&str>, to: Option<&str>| Change::Updated {
            mod_id: "1".into(), name: "Mod".into(), from: from.map(str::to_string), to: to.map(str::to_string), nexus_mod_id: None,
        };
        let none = BTreeMap::new();
        assert_eq!(render(&[up(Some("1.0"), None)], &none, None), "### Updated\n- Mod");
        assert_eq!(render(&[up(None, Some("2.0"))], &none, None), "### Updated\n- Mod to 2.0");
    }

    #[test]
    fn fit_cuts_on_a_line_boundary() {
        assert_eq!(fit("short".into()), "short");
        let notes = (0..5000).map(|i| format!("- line number {}", i)).collect::<Vec<_>>().join("\n");
        let got = fit(notes);
        assert!(got.encode_utf16().count() <= 32000);
        assert!(got.ends_with("\n- ...and more"));
        let body = got.strip_suffix("\n- ...and more").unwrap();
        assert!(body.lines().all(|l| l.starts_with("- line number ")));
        assert!(body.lines().count() > 1000);
    }

    #[test]
    fn fit_history_drops_oldest() {
        let entry = |i: usize, notes: String| VersionEntry { version: format!("v{}", i), published_at: i as u64, notes };
        let many: Vec<_> = (0..150).map(|i| entry(i, "n".into())).collect();
        let got = fit_history(many);
        assert_eq!(got.len(), 99);
        assert_eq!(got[0].version, "v0");
        let big: Vec<_> = (0..40).map(|i| entry(i, "x".repeat(32000))).collect();
        let got = fit_history(big);
        assert!(serde_json::to_vec(&got).unwrap().len() <= 1000000);
        assert!(got.len() < 40 && got.len() > 20);
        assert_eq!(got[0].version, "v0");
    }
}
