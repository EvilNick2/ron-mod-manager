use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::AppHandle;

use crate::accounts::{normalize_base, Conn};
use crate::config::{self, AppConfig};
use crate::packs;
use crate::vfs;

const SERVER_KIND: &str = "server";
const DEFAULT_APP_ID: u64 = 1144200;

enum Token<'a> {
    Text(&'a str),
    Open,
    Close,
}

struct Tokens<'a> {
    vdf: &'a str,
    at: usize,
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let bytes = self.vdf.as_bytes();
        while let Some(b) = bytes.get(self.at) {
            self.at += 1;
            match b {
                b'{' => return Some(Token::Open),
                b'}' => return Some(Token::Close),
                b'"' => {
                    let start = self.at;
                    loop {
                        match bytes.get(self.at)? {
                            b'\\' => self.at += 2,
                            b'"' => {
                                self.at += 1;
                                return self.vdf.get(start..self.at - 1).map(Token::Text);
                            }
                            _ => self.at += 1,
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }
}

/// The only value taken from localconfig.vdf, which also holds other Steam data such as app tickets
pub fn last_played(vdf: &str, app_id: u64) -> Option<u64> {
    let app = app_id.to_string();
    let path = ["UserLocalConfigStore", "Software", "Valve", "Steam", "apps", app.as_str()];
    let mut tokens = Tokens { vdf, at: 0 };
    let mut depth = 0usize;
    let mut matched = 0usize;
    while let Some(token) = tokens.next() {
        let (key, after) = match token {
            Token::Text(key) => (Some(key), tokens.next()?),
            other => (None, other),
        };
        match after {
            Token::Open => {
                if depth == matched && key.is_some_and(|k| path.get(depth).is_some_and(|p| p.eq_ignore_ascii_case(k))) {
                    matched += 1;
                }
                depth += 1;
            }
            Token::Close => {
                depth = depth.saturating_sub(1);
                matched = matched.min(depth);
            }
            Token::Text(value) => {
                if matched == path.len() && depth == matched && key.is_some_and(|k| k.eq_ignore_ascii_case("LastPlayed")) {
                    return value.parse().ok().filter(|n| *n > 0);
                }
            }
        }
    }
    None
}

/// The newest play across every Steam account on the machine
pub fn newest_play(steam_roots: &[PathBuf], app_id: u64) -> Option<u64> {
    steam_roots
        .iter()
        .filter_map(|root| fs::read_dir(root.join("userdata")).ok())
        .flatten()
        .flatten()
        .filter_map(|account| fs::read(account.path().join("config").join("localconfig.vdf")).ok())
        .filter_map(|bytes| last_played(&String::from_utf8_lossy(&bytes), app_id))
        .max()
}

#[cfg(windows)]
fn registry_steam_root() -> Option<PathBuf> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).open_subkey("Software\\Valve\\Steam").ok()?;
    key.get_value::<String, _>("SteamPath").ok().map(|p| PathBuf::from(p.replace('/', "\\")))
}

pub fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(windows)]
    {
        roots.extend(registry_steam_root());
        if let Ok(program_files) = std::env::var("ProgramFiles(x86)") {
            roots.push(PathBuf::from(program_files).join("Steam"));
        }
        if let Ok(program_files) = std::env::var("ProgramFiles") {
            roots.push(PathBuf::from(program_files).join("Steam"));
        }
    }

    #[cfg(unix)]
    {
        if let Ok(home) = std::env::var("HOME") {
            roots.push(PathBuf::from(&home).join(".local/share/Steam"));
            roots.push(PathBuf::from(&home).join(".steam/steam"));
            roots.push(PathBuf::from(&home).join(".steam/root"));
            roots.push(PathBuf::from(&home).join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        }
    }

    roots
}

pub fn enabled(config: &AppConfig) -> bool {
    config.report_pack_use != Some(false)
}

pub fn active_pack_id(config: &AppConfig) -> Option<String> {
    let id = config.active_preset.as_deref()?.strip_prefix("pack:")?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Turning reporting back on starts from now, so plays made while it was off stay private.
/// Steam can write such a play late, which is why this does not use the file's newest value.
fn set_reporting(config: &mut AppConfig, on: bool, now: u64) {
    if on && config.report_pack_use == Some(false) {
        config.last_reported_play = config.last_reported_play.max(Some(now));
    }
    config.report_pack_use = Some(on);
}

// None for a local-only or Drive pack, which has no server to tell
fn report_base(mods_dir: &Path, pack_id: &str) -> Option<String> {
    packs::parse_key(&format!("{}/1", pack_id)).ok()?;
    let manifest = packs::read_manifest(&mods_dir.join(pack_id))?;
    let linked = manifest.source.filter(|s| s.kind == SERVER_KIND).and_then(|s| s.server);
    linked.or(manifest.published.and_then(|p| p.server))
}

/// Reports `play` for the active pack when it is newer than the last one reported, and stores it.
/// A failed call stores nothing, so it is tried again on the next open. Returns whether a report went through.
pub fn report_for_open(config: &mut AppConfig, mods_dir: &Path, play: Option<u64>, connect: impl Fn(&str) -> Option<Conn>) -> bool {
    if !enabled(config) {
        return false;
    }
    let Some(play) = play.filter(|p| config.last_reported_play.is_none_or(|last| *p > last)) else { return false };
    let Some(pack_id) = active_pack_id(config) else { return false };
    let Some(conn) = report_base(mods_dir, &pack_id).and_then(|base| connect(&base)) else { return false };
    if conn.call(conn.server().report_used(&pack_id, play)).is_err() {
        return false;
    }
    config.last_reported_play = Some(play);
    true
}

pub fn note_downloaded(conn: &Conn, pack_id: &str, enabled: bool) {
    if enabled {
        let _ = conn.call(conn.server().report_downloaded(pack_id));
    }
}

/// With no Steam file to read, pressing Launch is the only sign of a play
fn launch_play(steam_roots: &[PathBuf], app_id: u64, now: u64) -> Option<u64> {
    newest_play(steam_roots, app_id).is_none().then_some(now)
}

fn connect_live(base: &str) -> Option<Conn> {
    let conn = Conn::live(&normalize_base(base).ok()?);
    conn.signed_in().then_some(conn)
}

fn app_id(config: &AppConfig) -> u64 {
    config.game_app_id.unwrap_or(DEFAULT_APP_ID)
}

// Only the one field is saved, over a config read after the network call, so changes made during it survive
fn report_and_save(app: &AppHandle, config: &mut AppConfig, play: Option<u64>) -> Result<(), String> {
    let mods_dir = vfs::get_mods_dir(app)?;
    if !report_for_open(config, &mods_dir, play, connect_live) {
        return Ok(());
    }
    let mut latest = config::load_config(app.clone())?;
    latest.last_reported_play = latest.last_reported_play.max(config.last_reported_play);
    config::save_config(app.clone(), latest)
}

fn now_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// After a whole pack came down. Never holds up or fails the caller.
pub fn report_download(app: &AppHandle, base: &str, pack_id: &str) {
    let on = config::load_config(app.clone()).is_ok_and(|c| enabled(&c));
    let (base, pack_id) = (base.to_string(), pack_id.to_string());
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(conn) = connect_live(&base) {
            note_downloaded(&conn, &pack_id, on);
        }
    });
}

/// After the game was started from the app. Never holds up or fails the caller.
pub fn report_launch(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Ok(mut config) = config::load_config(app.clone()) else { return };
        if !enabled(&config) {
            return;
        }
        let play = launch_play(&steam_roots(), app_id(&config), now_seconds());
        if let Err(e) = report_and_save(&app, &mut config, play) {
            eprintln!("[USAGE] Could not save the last reported play: {}", e);
        }
    });
}

#[tauri::command]
pub fn set_report_pack_use(app_handle: AppHandle, on: bool) -> Result<(), String> {
    let mut config = config::load_config(app_handle.clone())?;
    set_reporting(&mut config, on, now_seconds());
    config::save_config(app_handle, config)
}

#[tauri::command]
pub async fn report_pack_use_on_open(app_handle: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut config = config::load_config(app_handle.clone())?;
        if !enabled(&config) {
            return Ok(());
        }
        let play = newest_play(&steam_roots(), app_id(&config));
        report_and_save(&app_handle, &mut config, play)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::MemoryStore;
    use crate::packs::{write_manifest, PackManifest, PackSource, Published};
    use crate::server::fake::{FakeServer, BASE};
    use crate::server::RemoteManifest;
    use std::cell::RefCell;
    use std::sync::Arc;

    const APP: u64 = 1144200;
    const PACK: &str = "Casual-Shitters";
    const VIEWER: &str = "viewer";
    const PLAY: u64 = 1_699_000_000;
    const TICKET: &str = "380000000400000000000000deadbeef";

    fn fixture(last_played: &str) -> String {
        format!(
            r#""UserLocalConfigStore"
{{
	"streaming"
	{{
		"ClientID"		"123"
	}}
	"apptickets"
	{{
		"{APP}"		"{TICKET}"
	}}
	"Software"
	{{
		"Valve"
		{{
			"Steam"
			{{
				"apps"
				{{
					"480"
					{{
						"LastPlayed"		"1000"
					}}
					"{APP}"
					{{
						"Playtime"		"42"
						{last_played}
					}}
				}}
			}}
		}}
	}}
}}
"#
        )
    }

    fn played(n: u64) -> String {
        fixture(&format!(r#""LastPlayed"		"{}""#, n))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ron-usage-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put_account(root: &Path, id: &str, vdf: &str) {
        let dir = root.join("userdata").join(id).join("config");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("localconfig.vdf"), vdf).unwrap();
    }

    fn put_pack(mods: &Path, id: &str, edit: impl FnOnce(&mut PackManifest)) {
        let dir = mods.join(id);
        fs::create_dir_all(&dir).unwrap();
        let mut man = PackManifest { pack_id: id.into(), name: id.into(), ..Default::default() };
        edit(&mut man);
        write_manifest(&dir, &man).unwrap();
    }

    fn linked(m: &mut PackManifest) {
        m.source = Some(PackSource { kind: "server".into(), server: Some(BASE.into()), ..Default::default() });
    }

    fn server() -> FakeServer {
        let owner = FakeServer::new();
        let manifest = RemoteManifest {
            format: 1,
            pack_id: PACK.into(),
            name: PACK.into(),
            version: "1.0".into(),
            status: "published".into(),
            published_at: 1,
            published_by: None,
            mods: Vec::new(),
        };
        owner.seed_pack(crate::server::fake::OWNER, &manifest, "");
        let viewer = owner.add_user(VIEWER, "Viewer", None);
        owner.add_viewer(PACK, VIEWER);
        viewer
    }

    fn conn(server: &FakeServer) -> Conn {
        Conn::new(server.as_user(VIEWER), Arc::new(MemoryStore::default()))
    }

    fn config() -> AppConfig {
        AppConfig { active_preset: Some(format!("pack:{}", PACK)), ..Default::default() }
    }

    #[test]
    fn reads_last_played_for_the_app() {
        assert_eq!(last_played(&fixture(r#""LastPlayed"		"1792871734""#), APP), Some(1792871734));
    }

    #[test]
    fn path_matches_in_any_case() {
        let vdf = format!(
            r#""userlocalconfigstore" {{ "SOFTWARE" {{ "valve" {{ "STEAM" {{ "Apps" {{ "{APP}" {{ "lastplayed" "5" }} }} }} }} }} }}"#
        );
        assert_eq!(last_played(&vdf, APP), Some(5));
    }

    #[test]
    fn ignores_the_ticket_entry_with_the_same_key() {
        assert_eq!(last_played(&played(1792871734), APP), Some(1792871734));
        assert_eq!(last_played(&fixture(""), APP), None);
        let ticket_only = format!(r#""UserLocalConfigStore" {{ "{APP}" "{TICKET}" }}"#);
        assert_eq!(last_played(&ticket_only, APP), None);
    }

    #[test]
    fn only_the_exact_path_counts() {
        let elsewhere = format!(
            r#""UserLocalConfigStore" {{ "apps" {{ "{APP}" {{ "LastPlayed" "7" }} }} "Software" {{ "Valve" {{ "Steam" {{ "friends" {{ "apps" {{ "{APP}" {{ "LastPlayed" "8" }} }} }} }} }} }} }}"#
        );
        assert_eq!(last_played(&elsewhere, APP), None);
        let closed_then_reopened = format!(
            r#""UserLocalConfigStore" {{ "Software" {{ "Valve" {{ "Steam" {{ "apps" {{ "480" {{ "LastPlayed" "1" }} }} "apps" {{ "{APP}" {{ "LastPlayed" "9" }} }} }} }} }} }}"#
        );
        assert_eq!(last_played(&closed_then_reopened, APP), Some(9));
    }

    #[test]
    fn a_value_that_is_not_a_play_time_is_none() {
        for bad in ["0", "abc", "-5", "", "99999999999999999999999"] {
            assert_eq!(last_played(&fixture(&format!(r#""LastPlayed" "{}""#, bad)), APP), None, "{:?}", bad);
        }
    }

    #[test]
    fn garbage_is_none() {
        let full = played(1792871734);
        for cut in 0..full.len() - 60 {
            let _ = last_played(&full[..cut], APP);
        }
        for bad in [
            "",
            "}}}}}}",
            "{{{{{{",
            "\"",
            "\"\\",
            "\"UserLocalConfigStore\"",
            "\"UserLocalConfigStore\" {",
            "\u{0}\u{1}\u{fffd}\u{fffd}PNG\r\n\u{1a}\u{0}\"\u{fffd}{",
        ] {
            assert_eq!(last_played(bad, APP), None, "{:?}", bad);
        }
    }

    #[test]
    fn newest_across_accounts() {
        let root = scratch("accounts");
        let other = scratch("accounts2");
        put_account(&root, "1", &played(100));
        put_account(&root, "2", &played(300));
        put_account(&root, "3", "not a vdf");
        put_account(&other, "4", &played(200));
        let missing = root.join("nowhere");
        assert_eq!(newest_play(&[missing.clone(), root.clone(), other.clone()], APP), Some(300));
        assert_eq!(newest_play(&[missing], APP), None);
    }

    #[test]
    fn a_file_that_is_not_utf8_is_still_read() {
        let root = scratch("bytes");
        put_account(&root, "1", &played(100));
        let path = root.join("userdata/1/config/localconfig.vdf");
        let mut bytes = b"\"junk\" \"\xff\xfe\"\n".to_vec();
        bytes.extend(fs::read(&path).unwrap());
        fs::write(&path, bytes).unwrap();
        assert_eq!(newest_play(&[root], APP), Some(100));
    }

    #[test]
    fn launch_reports_now_only_without_a_file() {
        let root = scratch("launch");
        assert_eq!(launch_play(&[root.clone()], APP, 777), Some(777));
        put_account(&root, "1", &played(100));
        assert_eq!(launch_play(&[root], APP, 777), None);
    }

    #[test]
    fn active_pack_id_reads_only_packs() {
        let mut c = AppConfig::default();
        assert_eq!(active_pack_id(&c), None);
        c.active_preset = Some("My Preset".into());
        assert_eq!(active_pack_id(&c), None);
        c.active_preset = Some("pack:".into());
        assert_eq!(active_pack_id(&c), None);
        c.active_preset = Some("pack:Some-Pack".into());
        assert_eq!(active_pack_id(&c).as_deref(), Some("Some-Pack"));
    }

    #[test]
    fn reports_a_new_play_once() {
        let mods = scratch("once");
        put_pack(&mods, PACK, linked);
        let fake = server();
        let mut c = config();
        assert!(report_for_open(&mut c, &mods, Some(PLAY), |_| Some(conn(&fake))));
        assert_eq!(c.last_reported_play, Some(PLAY));
        assert_eq!(fake.last_used(PACK), Some(PLAY));
        assert!(!report_for_open(&mut c, &mods, Some(PLAY), |_| Some(conn(&fake))));
        assert!(!report_for_open(&mut c, &mods, Some(PLAY - 1), |_| Some(conn(&fake))));
        assert_eq!(fake.report_calls(), 1);
        assert!(report_for_open(&mut c, &mods, Some(PLAY + 1), |_| Some(conn(&fake))));
        assert_eq!(fake.last_used(PACK), Some(PLAY + 1));
    }

    #[test]
    fn no_play_sends_nothing() {
        let mods = scratch("noplay");
        put_pack(&mods, PACK, linked);
        let fake = server();
        let mut c = config();
        assert!(!report_for_open(&mut c, &mods, None, |_| Some(conn(&fake))));
        assert_eq!(fake.report_calls(), 0);
    }

    #[test]
    fn off_sends_nothing() {
        let mods = scratch("off");
        put_pack(&mods, PACK, linked);
        let fake = server();
        let mut c = AppConfig { report_pack_use: Some(false), ..config() };
        assert!(!enabled(&c));
        assert!(!report_for_open(&mut c, &mods, Some(PLAY), |_| Some(conn(&fake))));
        assert_eq!(c.last_reported_play, None);
        note_downloaded(&conn(&fake), PACK, enabled(&c));
        assert_eq!(fake.report_calls(), 0);
        assert_eq!(fake.download_count(PACK), 0);
    }

    #[test]
    fn a_play_from_before_turning_back_on_is_not_sent() {
        let mods = scratch("reenable");
        put_pack(&mods, PACK, linked);
        let fake = server();
        let mut c = AppConfig { last_reported_play: Some(5), ..config() };
        set_reporting(&mut c, false, PLAY - 100);
        assert_eq!(c.report_pack_use, Some(false));
        assert_eq!(c.last_reported_play, Some(5));
        set_reporting(&mut c, true, PLAY);
        assert!(enabled(&c));
        assert_eq!(c.last_reported_play, Some(PLAY));
        assert!(!report_for_open(&mut c, &mods, Some(PLAY - 50), |_| Some(conn(&fake))));
        assert!(!report_for_open(&mut c, &mods, Some(PLAY), |_| Some(conn(&fake))));
        assert_eq!(fake.report_calls(), 0);
        assert!(report_for_open(&mut c, &mods, Some(PLAY + 1), |_| Some(conn(&fake))));
        assert_eq!(fake.last_used(PACK), Some(PLAY + 1));
    }

    #[test]
    fn turning_back_on_never_moves_the_last_play_backwards() {
        let mut c = AppConfig { report_pack_use: Some(false), last_reported_play: Some(PLAY), ..Default::default() };
        set_reporting(&mut c, true, PLAY - 1);
        assert_eq!(c.last_reported_play, Some(PLAY));
    }

    #[test]
    fn only_the_change_from_off_moves_the_last_play() {
        let mut c = AppConfig { last_reported_play: Some(5), ..Default::default() };
        set_reporting(&mut c, true, PLAY);
        assert_eq!((c.report_pack_use, c.last_reported_play), (Some(true), Some(5)));
        set_reporting(&mut c, true, PLAY);
        assert_eq!(c.last_reported_play, Some(5));
    }

    #[test]
    fn unset_setting_means_on() {
        assert!(enabled(&AppConfig::default()));
        assert!(enabled(&AppConfig { report_pack_use: Some(true), ..Default::default() }));
        let old: AppConfig = serde_json::from_str(r#"{"game_path":null}"#).unwrap();
        assert_eq!((old.report_pack_use, old.last_reported_play), (None, None));
    }

    #[test]
    fn failed_report_retries_next_open() {
        let mods = scratch("retry");
        put_pack(&mods, PACK, linked);
        let fake = server();
        fake.fail_daily_cap(1);
        let mut c = AppConfig { last_reported_play: Some(5), ..config() };
        assert!(!report_for_open(&mut c, &mods, Some(PLAY), |_| Some(conn(&fake))));
        assert_eq!(c.last_reported_play, Some(5));
        assert_eq!(fake.last_used(PACK), None);
        assert!(report_for_open(&mut c, &mods, Some(PLAY), |_| Some(conn(&fake))));
        assert_eq!(c.last_reported_play, Some(PLAY));
        assert_eq!(fake.last_used(PACK), Some(PLAY));
    }

    #[test]
    fn local_and_drive_packs_send_nothing() {
        let mods = scratch("local");
        put_pack(&mods, "Local-Only", |_| {});
        put_pack(&mods, "From-Drive", |m| {
            m.source = Some(PackSource { kind: "drive".into(), folder_id: Some("F".into()), ..Default::default() });
        });
        put_pack(&mods, "Drive-Era", |m| m.published = Some(Published { server: None, dirty: false }));
        let fake = server();
        for id in ["Local-Only", "From-Drive", "Drive-Era", "Not-Installed", "../escape", "12345"] {
            let asked = RefCell::new(0);
            let mut c = AppConfig { active_preset: Some(format!("pack:{}", id)), ..Default::default() };
            let sent = report_for_open(&mut c, &mods, Some(PLAY), |_| {
                *asked.borrow_mut() += 1;
                Some(conn(&fake))
            });
            assert!(!sent, "{}", id);
            assert_eq!(*asked.borrow(), 0, "{}", id);
            assert_eq!(c.last_reported_play, None, "{}", id);
        }
        let mut c = AppConfig { active_preset: Some("My Preset".into()), ..Default::default() };
        assert!(!report_for_open(&mut c, &mods, Some(PLAY), |_| Some(conn(&fake))));
        assert_eq!(fake.report_calls(), 0);
    }

    #[test]
    fn base_comes_from_the_source_or_the_publish() {
        let mods = scratch("base");
        put_pack(&mods, PACK, |m| m.published = Some(Published { server: Some(BASE.into()), dirty: true }));
        let fake = server();
        let bases = RefCell::new(Vec::new());
        let mut c = config();
        assert!(report_for_open(&mut c, &mods, Some(PLAY), |b| {
            bases.borrow_mut().push(b.to_string());
            Some(conn(&fake))
        }));
        assert_eq!(*bases.borrow(), [BASE]);

        let mut c = AppConfig { last_reported_play: None, ..config() };
        assert!(!report_for_open(&mut c, &mods, Some(PLAY), |_| None));
        assert_eq!(c.last_reported_play, None);
    }

    #[test]
    fn download_is_noted_when_on() {
        let fake = server();
        note_downloaded(&conn(&fake), PACK, true);
        assert_eq!(fake.download_count(PACK), 1);
        assert!(fake.last_downloaded(PACK).is_some());
    }

    #[test]
    fn download_error_is_dropped() {
        let fake = server();
        fake.fail_daily_cap(1);
        note_downloaded(&conn(&fake), PACK, true);
        assert_eq!(fake.download_count(PACK), 0);
        note_downloaded(&conn(&fake), "Not-A-Pack", true);
    }

}
