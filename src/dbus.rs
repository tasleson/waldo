// SPDX-License-Identifier: MIT
use zbus::Connection;
use zbus::zvariant::OwnedObjectPath;

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    fn get_session_by_pid(&self, pid: u32) -> zbus::Result<OwnedObjectPath>;

    fn list_sessions(&self) -> zbus::Result<Vec<(String, u32, String, String, OwnedObjectPath)>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
pub trait Login1Session {
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;
}

pub async fn discover_session(conn: &Connection) -> anyhow::Result<OwnedObjectPath> {
    let manager = Login1ManagerProxy::new(conn).await?;

    match manager.get_session_by_pid(std::process::id()).await {
        Ok(path) => {
            tracing::info!("Found session via PID: {path}");
            return Ok(path);
        }
        Err(e) => {
            tracing::warn!("GetSessionByPID failed ({e}), falling back to ListSessions");
        }
    }

    let our_uid = unsafe { libc::getuid() };
    let sessions = manager.list_sessions().await?;
    let ours: Vec<_> = sessions
        .iter()
        .filter(|(_id, uid, _user, _seat, _path)| *uid == our_uid)
        .collect();

    // Prefer sessions with a seat — those are graphical sessions that
    // support screen locking. The "manager" class session has no seat.
    if let Some((id, _uid, _user, _seat, path)) = ours
        .iter()
        .find(|(_id, _uid, _user, seat, _path)| !seat.is_empty())
        .or(ours.first())
        .or(sessions.first().as_ref())
    {
        tracing::info!("Using session {id} at {path}");
        return Ok(path.clone());
    }

    anyhow::bail!("no active logind sessions found")
}

pub async fn session_proxy(
    conn: &Connection,
    path: OwnedObjectPath,
) -> zbus::Result<Login1SessionProxy<'static>> {
    Login1SessionProxy::builder(conn).path(path)?.build().await
}
