//! Session drivers: load the session from the request cookies, save it into `Set-Cookie`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cookie::{Cookie, CookieJar, SameSite};
use http::{HeaderMap, HeaderValue, header};
use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};
use sea_orm::{ConnectionTrait, Value};

use super::{Payload, Queued, Session, State};
use crate::app::App;
use crate::crypto::Keys;
use crate::error::{Error, Result};

/// The table of the database driver.
pub(crate) const SESSIONS_TABLE: &str = "sessions";

/// A browser keeps cookies up to about 4 KB.
const COOKIE_LIMIT: usize = 4093;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Driver {
    Cookie,
    Database,
    File,
}

impl Driver {
    pub(crate) fn from_settings(name: &str) -> Result<Self> {
        match name {
            "cookie" | "" => Ok(Self::Cookie),
            "database" => Ok(Self::Database),
            "file" => Ok(Self::File),
            other => Err(Error::internal(format!(
                "SESSION_DRIVER must be `cookie`, `database` or `file`, not `{other}`"
            ))),
        }
    }
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The cookies the request carries.
pub(crate) fn request_jar(headers: &HeaderMap) -> CookieJar {
    let mut jar = CookieJar::new();
    for value in headers.get_all(header::COOKIE) {
        let Ok(text) = value.to_str() else { continue };
        for cookie in Cookie::split_parse(text.to_owned()).flatten() {
            jar.add_original(cookie);
        }
    }
    jar
}

/// The decrypted value of cookie `name`, if it is there and authentic.
pub(crate) fn decrypt(jar: &CookieJar, keys: &Keys, name: &str) -> Option<String> {
    jar.private(&keys.cookie)
        .get(name)
        .map(|c| c.value().to_owned())
}

fn fresh() -> Result<State> {
    let payload = Payload {
        issued: Some(now_secs()),
        ..Payload::default()
    };
    Ok(State::new(crate::crypto::random_token(40)?, payload))
}

/// The session cookie's name: `SESSION_COOKIE`, with the `__Host-` prefix when cookies are
/// `Secure` (an https `APP_URL`), so a sibling subdomain or a plain-HTTP page cannot set it.
pub(crate) fn session_cookie_name(settings: &crate::config::Settings) -> String {
    if settings.secure_cookies() {
        format!("__Host-{}", settings.session_cookie)
    } else {
        settings.session_cookie.clone()
    }
}

/// Load the visitor's session; a missing, tampered, idle-expired or too old (past
/// `SESSION_ABSOLUTE_LIFETIME`) one gives a new, empty session.
pub(crate) async fn load(app: &App, keys: &Keys, driver: Driver, jar: &CookieJar) -> Result<State> {
    let Some(mut state) = load_existing(app, keys, driver, jar).await? else {
        return fresh();
    };
    let absolute = app.settings().session_absolute_lifetime.as_secs();
    let now = now_secs();
    match state.payload.issued {
        // A session saved before the clock existed starts it now.
        None => state.payload.issued = Some(now),
        Some(issued) if absolute > 0 && issued.saturating_add(absolute) <= now => {
            let mut started = fresh()?;
            // The database and file drivers delete the old row or file when the new one is saved.
            started.previous_id = Some(state.id);
            return Ok(started);
        }
        Some(_) => {}
    }
    state.loaded = true;
    Ok(state)
}

/// The session the request's cookie names, read without changing anything: `None` when it is missing, tampered,
/// idle-expired or past `SESSION_ABSOLUTE_LIFETIME` (where [`load`] would start a new one).
pub(crate) async fn peek(
    app: &App,
    keys: &Keys,
    driver: Driver,
    jar: &CookieJar,
) -> Result<Option<State>> {
    let Some(mut state) = load_existing(app, keys, driver, jar).await? else {
        return Ok(None);
    };
    let absolute = app.settings().session_absolute_lifetime.as_secs();
    if let Some(issued) = state.payload.issued
        && absolute > 0
        && issued.saturating_add(absolute) <= now_secs()
    {
        return Ok(None);
    }
    state.loaded = true;
    Ok(Some(state))
}

/// The session the request's cookie names, if it is there and not idle-expired.
async fn load_existing(
    app: &App,
    keys: &Keys,
    driver: Driver,
    jar: &CookieJar,
) -> Result<Option<State>> {
    let fresh = || Ok(None);
    let settings = app.settings();
    let Some(value) = decrypt(jar, keys, &session_cookie_name(settings)) else {
        return fresh();
    };
    match driver {
        Driver::Cookie => {
            let Ok(payload) = serde_json::from_str::<Payload>(&value) else {
                return fresh();
            };
            if payload.expires.is_none_or(|e| e <= now_secs()) {
                return fresh();
            }
            let id = match payload.sid.clone() {
                Some(id) => id,
                None => crate::crypto::random_token(40)?,
            };
            Ok(Some(State::new(id, payload)))
        }
        Driver::Database => {
            let db = app.db()?;
            let lifetime = settings.session_lifetime.as_secs();
            let stmt = db.conn().get_database_backend().build(
                &Query::select()
                    .columns([Alias::new("payload"), Alias::new("last_activity")])
                    .from(Alias::new(SESSIONS_TABLE))
                    .and_where(Expr::col(Alias::new("id")).eq(value.clone()))
                    .to_owned(),
            );
            let Some(row) = db.conn().query_one_raw(stmt).await? else {
                return fresh();
            };
            let last: i64 = row.try_get("", "last_activity")?;
            let payload: String = row.try_get("", "payload")?;
            let last = u64::try_from(last).unwrap_or(0);
            if last.saturating_add(lifetime) <= now_secs() {
                return fresh();
            }
            let payload = serde_json::from_str::<Payload>(&payload).unwrap_or_default();
            Ok(Some(State::new(value, payload)))
        }
        Driver::File => {
            if !is_session_id(&value) {
                return fresh();
            }
            let path = sessions_dir(app).join(&value);
            let lifetime = settings.session_lifetime;
            let text = tokio::task::spawn_blocking(move || read_session_file(&path, lifetime))
                .await
                .map_err(|e| Error::internal(format!("reading the session failed: {e}")))??;
            let Some(text) = text else {
                return fresh();
            };
            let payload = serde_json::from_str::<Payload>(&text).unwrap_or_default();
            Ok(Some(State::new(value, payload)))
        }
    }
}

/// The file driver's directory: `storage/framework/sessions`.
pub(crate) fn sessions_dir(app: &App) -> PathBuf {
    app.settings()
        .storage_dir()
        .join("framework")
        .join("sessions")
}

/// Session ids are 40 characters from `[A-Za-z0-9]`; anything else never becomes a file name.
fn is_session_id(id: &str) -> bool {
    id.len() == 40 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// How long ago the file was last written, if its metadata says.
fn file_age(meta: &std::fs::Metadata) -> Option<Duration> {
    meta.modified()
        .ok()
        .map(|m| m.elapsed().unwrap_or_default())
}

/// The payload of a session file that is younger than `lifetime`; `None` when it is missing or
/// expired (the file's modification time is the session's last activity).
fn read_session_file(path: &Path, lifetime: Duration) -> Result<Option<String>> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if file_age(&meta).is_none_or(|age| age.as_secs() >= lifetime.as_secs()) {
        return Ok(None);
    }
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        // Swept or replaced between the two calls.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Write the session file: a temp file in the same directory, then a rename, so a reader sees
/// the old payload or the new one, never half of one. The file is readable by its owner only.
///
/// With `existing_only` (a session loaded from its file that kept its id) the file is written only while it is
/// still there: `Ok(false)` when it is gone (another request ended the session), and nothing is written.
fn write_session_file(
    dir: &Path,
    id: &str,
    previous: Option<&str>,
    text: &str,
    temp_name: &str,
    sweep: Option<Duration>,
    existing_only: bool,
) -> Result<bool> {
    use std::io::Write as _;
    if !is_session_id(id) {
        return Err(Error::internal("a session id has an unexpected form"));
    }
    std::fs::create_dir_all(dir)?;
    if let Some(lifetime) = sweep {
        sweep_sessions(dir, lifetime);
    }
    if let Some(previous) = previous.filter(|p| is_session_id(p)) {
        remove_if_present(&dir.join(previous))?;
    }
    let target = dir.join(id);
    let temp = dir.join(temp_name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let written = options.open(&temp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    // Checked again right before the rename (the write above takes time): the window left is between two calls.
    let ended = written
        .and_then(|()| Ok(existing_only && !target.try_exists()?))
        .and_then(|ended| {
            if !ended {
                std::fs::rename(&temp, &target)?;
            }
            Ok(ended)
        });
    match ended {
        Ok(false) => Ok(true),
        Ok(true) => {
            let _ = std::fs::remove_file(&temp);
            Ok(false)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e.into())
        }
    }
}

/// Delete session files (and temp files a crash left) idle for at least `lifetime`.
fn sweep_sessions(dir: &Path, lifetime: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let ours = is_session_id(name)
            || name
                .strip_suffix(".tmp")
                .and_then(|n| n.split_once('.'))
                .is_some_and(|(id, _)| is_session_id(id));
        let expired = entry
            .metadata()
            .ok()
            .and_then(|m| file_age(&m))
            .is_some_and(|age| age.as_secs() > lifetime.as_secs());
        if ours && expired {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Expired sessions go away on about one request in a hundred.
fn prune_now() -> Result<bool> {
    Ok(crate::crypto::random_bytes(1)?
        .first()
        .is_some_and(|b| *b < 3))
}

fn base_cookie(name: String, value: String, secure: bool) -> Cookie<'static> {
    Cookie::build((name, value))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure)
        .build()
}

/// The `XSRF-TOKEN` cookie (D-276): the masked CSRF token (a fresh mask per response, base64url
/// so it needs no cookie escaping), readable by the page's JavaScript (not `HttpOnly`), `Lax`,
/// `Secure` under an https `APP_URL`, for the browser session (no `Max-Age`).
pub(crate) fn set_xsrf_cookie(app: &App, token: String, headers: &mut HeaderMap) -> Result<()> {
    let mut cookie = base_cookie(
        super::web::XSRF_COOKIE.to_owned(),
        token,
        app.settings().secure_cookies(),
    );
    cookie.set_http_only(false);
    let value = HeaderValue::from_str(&cookie.to_string())
        .map_err(|_| Error::internal("a cookie is not a valid header value"))?;
    headers.append(header::SET_COOKIE, value);
    Ok(())
}

fn max_age(d: Duration) -> cookie::time::Duration {
    cookie::time::Duration::seconds(i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Delete the stored session `id` (database row or file); the cookie driver stores nothing.
async fn delete_session(app: &App, driver: Driver, id: String) -> Result<()> {
    match driver {
        Driver::Cookie => Ok(()),
        Driver::Database => {
            let db = app.db()?;
            let delete = Query::delete()
                .from_table(Alias::new(SESSIONS_TABLE))
                .and_where(Expr::col(Alias::new("id")).eq(id))
                .to_owned();
            db.conn()
                .execute_raw(db.conn().get_database_backend().build(&delete))
                .await?;
            Ok(())
        }
        Driver::File => {
            if !is_session_id(&id) {
                return Ok(());
            }
            let path = sessions_dir(app).join(id);
            tokio::task::spawn_blocking(move || remove_if_present(&path))
                .await
                .map_err(|e| Error::internal(format!("deleting the session failed: {e}")))?
        }
    }
}

/// Save the session (after the handler) and write its cookies into `headers`.
pub(crate) async fn save(
    app: &App,
    keys: &Keys,
    driver: Driver,
    session: &Session,
    headers: &mut HeaderMap,
) -> Result<()> {
    let settings = app.settings();
    let secure = settings.secure_cookies();
    let lifetime = settings.session_lifetime;
    let empty_and_new = session.is_empty() && session.with_state(|s| !s.loaded);
    let (id, previous, mut payload, queued, loaded) = session.with_state(|s| {
        (
            s.id.clone(),
            s.previous_id.take(),
            s.payload.clone(),
            std::mem::take(&mut s.queued),
            s.loaded,
        )
    });
    // A session that came with the request and kept its id is only ever updated: when its row or file is gone, a
    // request that ran at the same time ended it (a logout, a new id at sign-in) and it must never be written back.
    // Only a new id (a new session, or one given a new id during this request) is created.
    let kept = loaded && previous.is_none();
    let now = now_secs();
    if empty_and_new {
        // Nothing to keep for a visitor who came without a session (a first visit, a bot, an
        // API-style client): no cookie, no row, no file. A replaced session is still removed.
        if let Some(previous) = previous {
            delete_session(app, driver, previous).await?;
        }
        return Ok(());
    }
    payload.issued.get_or_insert(now);
    let value = match driver {
        Driver::Cookie => {
            payload.expires = Some(now.saturating_add(lifetime.as_secs()));
            payload.sid = Some(id);
            // Nothing is stored on the server, so there is nothing a parallel request could have ended.
            Some(serde_json::to_string(&payload)?)
        }
        Driver::Database => {
            payload.expires = None;
            payload.sid = None;
            let db = app.db()?;
            let backend = db.conn().get_database_backend();
            if let Some(previous) = previous {
                let delete = Query::delete()
                    .from_table(Alias::new(SESSIONS_TABLE))
                    .and_where(Expr::col(Alias::new("id")).eq(previous))
                    .to_owned();
                db.conn().execute_raw(backend.build(&delete)).await?;
            }
            let text = serde_json::to_string(&payload)?;
            let now_i = i64::try_from(now).unwrap_or(i64::MAX);
            let update = Query::update()
                .table(Alias::new(SESSIONS_TABLE))
                .values([
                    (Alias::new("payload"), Value::from(text.clone()).into()),
                    (Alias::new("last_activity"), Value::from(now_i).into()),
                ])
                .and_where(Expr::col(Alias::new("id")).eq(id.clone()))
                .to_owned();
            // MySQL counts matched rows here (sqlx connects with `CLIENT_FOUND_ROWS`), so an unchanged payload in
            // the same second still counts as found.
            let updated = db.conn().execute_raw(backend.build(&update)).await?;
            let stored = updated.rows_affected() > 0;
            if !stored && !kept {
                let mut insert = Query::insert();
                insert
                    .into_table(Alias::new(SESSIONS_TABLE))
                    .columns([
                        Alias::new("id"),
                        Alias::new("payload"),
                        Alias::new("last_activity"),
                    ])
                    .values([
                        Value::from(id.clone()).into(),
                        Value::from(text).into(),
                        Value::from(now_i).into(),
                    ])
                    .map_err(|e| Error::internal(e.to_string()))?;
                db.conn().execute_raw(backend.build(&insert)).await?;
            }
            if prune_now()? {
                let cutoff = now_i.saturating_sub(i64::try_from(lifetime.as_secs()).unwrap_or(0));
                let prune = Query::delete()
                    .from_table(Alias::new(SESSIONS_TABLE))
                    .and_where(Expr::col(Alias::new("last_activity")).lt(cutoff))
                    .to_owned();
                db.conn().execute_raw(backend.build(&prune)).await?;
            }
            (stored || !kept).then_some(id)
        }
        Driver::File => {
            payload.expires = None;
            payload.sid = None;
            let text = serde_json::to_string(&payload)?;
            let dir = sessions_dir(app);
            let temp_name = format!("{id}.{}.tmp", crate::crypto::random_token(16)?);
            let sweep = prune_now()?.then_some(lifetime);
            let file_id = id.clone();
            let written = tokio::task::spawn_blocking(move || {
                write_session_file(
                    &dir,
                    &file_id,
                    previous.as_deref(),
                    &text,
                    &temp_name,
                    sweep,
                    kept,
                )
            })
            .await
            .map_err(|e| Error::internal(format!("saving the session failed: {e}")))??;
            written.then_some(id)
        }
    };

    let mut jar = CookieJar::new();
    let cookie_name = session_cookie_name(settings);
    // `None`: the session was ended by another request meanwhile; no cookie names it again.
    if let Some(value) = value {
        let mut cookie = base_cookie(cookie_name.clone(), value, secure);
        cookie.set_max_age(max_age(lifetime));
        jar.private_mut(&keys.cookie).add(cookie);
    }
    for q in queued {
        match q {
            Queued::Set {
                name,
                value,
                max_age: age,
            } => {
                let mut cookie = base_cookie(name, value, secure);
                cookie.set_max_age(max_age(age));
                jar.private_mut(&keys.cookie).add(cookie);
            }
            Queued::Remove { name } => {
                let mut cookie = base_cookie(name, String::new(), secure);
                cookie.set_max_age(cookie::time::Duration::ZERO);
                jar.add(cookie);
            }
        }
    }
    for cookie in jar.delta() {
        let text = cookie.to_string();
        if cookie.name() == cookie_name && text.len() > COOKIE_LIMIT {
            tracing::warn!(
                bytes = text.len(),
                "the session is larger than a browser keeps in one cookie; use SESSION_DRIVER=database or file"
            );
        }
        let value = HeaderValue::from_str(&text)
            .map_err(|_| Error::internal("a cookie is not a valid header value"))?;
        headers.append(header::SET_COOKIE, value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "abcdefghijABCDEFGHIJ0123456789abcdefghij";
    const OTHER: &str = "ZYXWVUTSRQzyxwvutsrq9876543210ZYXWVUTSRQ";
    const HOUR: Duration = Duration::from_secs(3600);

    fn age(path: &Path, by: Duration) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn the_driver_names() {
        assert_eq!(Driver::from_settings("file").unwrap(), Driver::File);
        assert_eq!(Driver::from_settings("").unwrap(), Driver::Cookie);
        let err = Driver::from_settings("redis").unwrap_err().to_string();
        assert!(err.contains("`cookie`, `database` or `file`"), "{err}");
    }

    #[test]
    fn only_session_ids_become_file_names() {
        assert!(is_session_id(ID));
        assert!(!is_session_id("../../../../etc/passwd"));
        assert!(!is_session_id(&"a".repeat(39)));
        assert!(!is_session_id(&format!("{}/", &ID[..39])));
        let dir = tempfile::tempdir().unwrap();
        assert!(write_session_file(dir.path(), "../x", None, "{}", "t.tmp", None, false).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_session_file_round_trips_and_replaces_the_previous_one() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        write_session_file(
            &sessions,
            ID,
            None,
            "one",
            &format!("{ID}.a.tmp"),
            None,
            false,
        )
        .unwrap();
        assert_eq!(
            read_session_file(&sessions.join(ID), HOUR)
                .unwrap()
                .as_deref(),
            Some("one")
        );
        write_session_file(
            &sessions,
            ID,
            None,
            "two",
            &format!("{ID}.b.tmp"),
            None,
            false,
        )
        .unwrap();
        assert_eq!(
            read_session_file(&sessions.join(ID), HOUR)
                .unwrap()
                .as_deref(),
            Some("two")
        );
        // Regenerating: the old id's file goes, the new one holds the data; no temp file is left.
        write_session_file(&sessions, OTHER, Some(ID), "two", "x.tmp", None, false).unwrap();
        let names: Vec<String> = std::fs::read_dir(&sessions)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, [OTHER]);
        assert_eq!(read_session_file(&sessions.join(ID), HOUR).unwrap(), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(sessions.join(OTHER))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn an_expired_session_file_reads_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        write_session_file(dir.path(), ID, None, "{}", "t.tmp", None, false).unwrap();
        let path = dir.path().join(ID);
        assert!(read_session_file(&path, HOUR).unwrap().is_some());
        assert_eq!(read_session_file(&path, Duration::ZERO).unwrap(), None);
        age(&path, 2 * HOUR);
        assert_eq!(read_session_file(&path, HOUR).unwrap(), None);
    }

    #[test]
    fn a_kept_session_is_never_written_back_once_its_file_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        // Still there: updated.
        write_session_file(p, ID, None, "one", "a.tmp", None, false).unwrap();
        assert!(write_session_file(p, ID, None, "two", "b.tmp", None, true).unwrap());
        assert_eq!(
            read_session_file(&p.join(ID), HOUR).unwrap().as_deref(),
            Some("two")
        );
        // Ended by another request (a logout removed it): not created again, no temp file left.
        std::fs::remove_file(p.join(ID)).unwrap();
        assert!(!write_session_file(p, ID, None, "three", "c.tmp", None, true).unwrap());
        assert_eq!(std::fs::read_dir(p).unwrap().count(), 0);
    }

    #[test]
    fn the_sweep_removes_only_expired_session_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        for name in [ID, OTHER, "keep.txt", ".gitignore"] {
            std::fs::write(p.join(name), "x").unwrap();
        }
        let crashed = format!("{ID}.abc.tmp");
        std::fs::write(p.join(&crashed), "x").unwrap();
        for name in [ID, "keep.txt", ".gitignore", crashed.as_str()] {
            age(&p.join(name), 3 * HOUR);
        }
        // A sweep during a write removes the expired ones before writing.
        let new = "NEWNEWNEWNnewnewnewn0000000000NEWNEWNEWN";
        write_session_file(p, new, None, "{}", "n.tmp", Some(HOUR), false).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(p)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        let mut expected = vec![".gitignore", new, OTHER, "keep.txt"];
        expected.sort_unstable();
        assert_eq!(names, expected);
    }
}
