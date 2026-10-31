use std::path::{Path, PathBuf};

use crate::accounts::{normalize_base, Conn};
use crate::packs::{self, PackManifest};
use crate::server::{AdminUser, Link, RemovedViewer, Viewer};
use crate::vfs;

const SERVER_KIND: &str = "server";

fn live(base: &str) -> Result<Conn, String> {
    Ok(Conn::live(&normalize_base(base)?))
}

fn local_pack(mods_dir: &Path, pack_id: &str) -> Result<(PathBuf, PackManifest), String> {
    let pack_dir = mods_dir.join(pack_id);
    let manifest = packs::parse_key(&format!("{}/1", pack_id))
        .ok()
        .and_then(|_| packs::read_manifest(&pack_dir))
        .ok_or_else(|| format!("No modpack folder for {:?}", pack_id))?;
    Ok((pack_dir, manifest))
}

/// The server a pack was installed from, None for a pack that was never linked to one
fn linked_server(m: &PackManifest) -> Option<String> {
    m.source.as_ref().filter(|s| s.kind == SERVER_KIND).and_then(|s| s.server.clone())
}

fn published_server(m: &PackManifest) -> Result<String, String> {
    m.published
        .as_ref()
        .and_then(|p| p.server.clone())
        .ok_or_else(|| format!("{} is not published to a pack server", m.name))
}

fn unlink(pack_dir: &Path, mut manifest: PackManifest) -> Result<(), String> {
    manifest.source = None;
    packs::write_manifest(pack_dir, &manifest)
}

/// Leaves on the server, then the pack stays installed as a plain local pack
fn leave_linked(conn: &Conn, pack_dir: &Path, manifest: PackManifest) -> Result<(), String> {
    match conn.call(conn.server().leave(&manifest.pack_id)) {
        Ok(()) => {}
        Err(e) if e.code == "pack_gone" => {}
        Err(e) => return Err(e.text(conn.base())),
    }
    unlink(pack_dir, manifest)
}

/// Deletes the pack on the server, then the local pack stays and counts as never published
fn delete_published(conn: &Conn, pack_dir: &Path, mut manifest: PackManifest) -> Result<(), String> {
    match conn.call(conn.server().delete_pack(&manifest.pack_id)) {
        Ok(()) => {}
        Err(e) if e.code == "pack_gone" => {}
        Err(e) => return Err(e.text(conn.base())),
    }
    manifest.published = None;
    packs::write_manifest(pack_dir, &manifest)
}

fn remove_viewer(conn: &Conn, pack_id: &str, user_id: &str) -> Result<RemovedViewer, String> {
    conn.text(conn.server().remove_viewer(pack_id, user_id))
}

fn access_of(conn: &Conn, pack_id: &str) -> Result<Option<String>, String> {
    let me = conn.text(conn.server().me())?;
    Ok(me.packs.into_iter().find(|p| p.id == pack_id).map(|p| p.access))
}

#[tauri::command]
pub async fn pack_invite_link(base: String, pack_id: String) -> Result<Link, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = live(&base)?;
        conn.text(conn.server().pack_invite(&pack_id))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn reset_pack_invite(base: String, pack_id: String) -> Result<Link, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = live(&base)?;
        conn.text(conn.server().reset_invite(&pack_id))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn pack_viewers(base: String, pack_id: String) -> Result<Vec<Viewer>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = live(&base)?;
        conn.text(conn.server().viewers(&pack_id))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn remove_pack_viewer(base: String, pack_id: String, user_id: String) -> Result<RemovedViewer, String> {
    tauri::async_runtime::spawn_blocking(move || remove_viewer(&live(&base)?, &pack_id, &user_id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn pack_editor_invite_link(base: String, pack_id: String) -> Result<Link, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = live(&base)?;
        conn.text(conn.server().editor_invite(&pack_id))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn pack_editors(base: String, pack_id: String) -> Result<Vec<Viewer>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = live(&base)?;
        conn.text(conn.server().editors(&pack_id))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn remove_pack_editor(base: String, pack_id: String, user_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = live(&base)?;
        conn.text(conn.server().remove_editor(&pack_id, &user_id))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn clear_publish_hold(base: String, pack_id: String) -> Result<u32, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = live(&base)?;
        conn.text(conn.server().clear_hold(&pack_id))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn pack_access(base: String, pack_id: String) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || access_of(&live(&base)?, &pack_id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn leave_pack(app_handle: tauri::AppHandle, pack_id: String) -> Result<(), String> {
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || {
        let (pack_dir, manifest) = local_pack(&mods_dir, &pack_id)?;
        match linked_server(&manifest) {
            Some(base) => leave_linked(&Conn::live(&base), &pack_dir, manifest),
            None if manifest.source.is_some() => unlink(&pack_dir, manifest),
            None => Err(format!("{} is not linked to a pack server", manifest.name)),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn delete_server_pack(app_handle: tauri::AppHandle, pack_id: String) -> Result<(), String> {
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || {
        let (pack_dir, manifest) = local_pack(&mods_dir, &pack_id)?;
        let base = published_server(&manifest)?;
        delete_published(&Conn::live(&base), &pack_dir, manifest)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn uploader_invite_link(base: String) -> Result<Link, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = live(&base)?;
        conn.text(conn.server().uploader_invite())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn server_users(base: String) -> Result<Vec<AdminUser>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = live(&base)?;
        conn.text(conn.server().users())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn remove_server_user(base: String, user_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = live(&base)?;
        conn.text(conn.server().remove_user(&user_id))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{save_session_in, session_in, MemoryStore, Session};
    use crate::packs::{PackSource, Published};
    use crate::server::fake::{FakeServer, BASE, OWNER};
    use crate::server::{RemoteManifest, User, FORMAT};
    use std::fs;
    use std::sync::Arc;

    const PACK: &str = "Casual-Shitters";
    const VIEWER: &str = "viewer";

    struct World {
        server: FakeServer,
        store: Arc<MemoryStore>,
        mods_dir: PathBuf,
    }

    impl World {
        fn new() -> World {
            let server = FakeServer::new();
            server.add_user(VIEWER, "Viewer", None);
            let store = Arc::new(MemoryStore::default());
            for id in [OWNER, VIEWER] {
                let session = Session { token: "tok".into(), user: User { id: id.into(), name: id.into(), role: None } };
                save_session_in(&*store, BASE, &session).unwrap();
            }
            let mods_dir = std::env::temp_dir().join(format!("ron-test-manage-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&mods_dir).unwrap();
            let manifest = RemoteManifest {
                format: FORMAT,
                pack_id: PACK.into(),
                name: "Casual Shitters".into(),
                version: "1.0.0".into(),
                status: "ready".into(),
                published_at: 1792364400,
                published_by: None,
                mods: vec![],
            };
            server.seed_pack(OWNER, &manifest, "First");
            server.add_viewer(PACK, VIEWER);
            World { server, store, mods_dir }
        }

        fn conn(&self, user: &str) -> Conn {
            Conn::new(self.server.as_user(user), self.store.clone())
        }

        /// A local pack with one file, edited by `edit` before it is written
        fn local(&self, edit: impl FnOnce(&mut PackManifest)) -> PathBuf {
            let dir = self.mods_dir.join(PACK);
            fs::create_dir_all(dir.join("3063")).unwrap();
            fs::write(dir.join("3063").join("a.pak"), "vests").unwrap();
            let mut m = PackManifest { pack_id: PACK.into(), name: "Casual Shitters".into(), ..Default::default() };
            edit(&mut m);
            packs::write_manifest(&dir, &m).unwrap();
            dir
        }
    }

    fn linked(m: &mut PackManifest) {
        m.source = Some(PackSource {
            kind: SERVER_KIND.into(),
            server: Some(BASE.into()),
            invite: Some("abc".into()),
            uploader: Some("Owner".into()),
            ..Default::default()
        });
    }

    #[test]
    fn leave_keeps_files_and_drops_source() {
        let w = World::new();
        let dir = w.local(linked);
        let (_, manifest) = local_pack(&w.mods_dir, PACK).unwrap();
        assert_eq!(linked_server(&manifest).as_deref(), Some(BASE));

        leave_linked(&w.conn(VIEWER), &dir, manifest).unwrap();

        assert!(dir.join("3063").join("a.pak").is_file());
        assert!(packs::read_manifest(&dir).unwrap().source.is_none());
        assert!(w.conn(OWNER).server().viewers(PACK).unwrap().is_empty());
    }

    #[test]
    fn leave_still_unlinks_when_the_server_forgot_the_pack() {
        let w = World::new();
        let dir = w.local(linked);
        w.conn(OWNER).server().delete_pack(PACK).unwrap();
        let (_, manifest) = local_pack(&w.mods_dir, PACK).unwrap();

        leave_linked(&w.conn(VIEWER), &dir, manifest).unwrap();

        assert!(packs::read_manifest(&dir).unwrap().source.is_none());
    }

    #[test]
    fn leave_failure_keeps_source_and_names_the_server() {
        let w = World::new();
        let dir = w.local(linked);
        w.server.expire_session();
        let (_, manifest) = local_pack(&w.mods_dir, PACK).unwrap();

        let err = leave_linked(&w.conn(VIEWER), &dir, manifest).unwrap_err();

        assert_eq!(err, "Signed out of packs.test. Sign in again in Settings.");
        assert!(packs::read_manifest(&dir).unwrap().source.is_some());
        assert!(session_in(&*w.store, BASE).is_none());
    }

    #[test]
    fn delete_clears_published() {
        let w = World::new();
        let dir = w.local(|m| m.published = Some(Published { server: Some(BASE.into()), dirty: true }));
        let (_, manifest) = local_pack(&w.mods_dir, PACK).unwrap();
        assert_eq!(published_server(&manifest).unwrap(), BASE);

        delete_published(&w.conn(OWNER), &dir, manifest).unwrap();

        assert!(!w.server.has_pack(PACK));
        assert!(packs::read_manifest(&dir).unwrap().published.is_none());
        assert!(dir.join("3063").join("a.pak").is_file());
    }

    #[test]
    fn delete_refuses_a_pack_that_was_never_published() {
        let w = World::new();
        w.local(|_| {});
        let (_, manifest) = local_pack(&w.mods_dir, PACK).unwrap();
        assert_eq!(published_server(&manifest).unwrap_err(), "Casual Shitters is not published to a pack server");
    }

    #[test]
    fn remove_viewer_reports_invite_reset() {
        let w = World::new();
        let owner = w.conn(OWNER);
        owner.server().pack_invite(PACK).unwrap();

        let removed = remove_viewer(&owner, PACK, VIEWER).unwrap();
        assert!(removed.invite_reset);
        assert!(owner.server().viewers(PACK).unwrap().is_empty());

        let again = remove_viewer(&owner, PACK, VIEWER).unwrap_err();
        assert_eq!(again, "This person is not on the server");
    }

    #[test]
    fn access_reports_role_or_none() {
        let w = World::new();
        assert_eq!(access_of(&w.conn(OWNER), PACK).unwrap().as_deref(), Some("uploader"));
        assert_eq!(access_of(&w.conn(VIEWER), PACK).unwrap().as_deref(), Some("viewer"));
        assert_eq!(access_of(&w.conn(VIEWER), "Other-Pack").unwrap(), None);
    }

    #[test]
    fn viewer_cannot_manage_access() {
        let w = World::new();
        let err = remove_viewer(&w.conn(VIEWER), PACK, OWNER).unwrap_err();
        assert_eq!(err, "Only the pack's creator or an editor can do this");
    }
}
