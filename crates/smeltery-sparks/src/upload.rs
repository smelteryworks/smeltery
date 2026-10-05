//! File uploads: `POST /_sparks/upload`, signed upload tokens, [`TemporaryUpload`] and storing files.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::Query;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use futures_util::StreamExt as _;
use http::{HeaderMap, StatusCode, header};
use serde::{Deserialize, Serialize};
use smeltery_core::http::ClientInfo;
use smeltery_core::session::Session;
use smeltery_core::validation::rules::label;
use smeltery_core::{App, Error, Result};
use tokio::io::AsyncWriteExt as _;

use crate::SparkCtx;
use crate::component::UploadRule;
use crate::runtime::Runtime;

/// The signing purpose of upload tokens.
const PURPOSE: &str = "sparks.upload";
/// Temp files older than this are deleted.
const TMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// How often an upload request looks for old temp files.
const CLEANUP_EVERY: u64 = 10 * 60;

/// A file uploaded through `wire:model` on a file input, waiting in `storage/framework/sparks/` until the
/// component stores it. Use it as an `Option<TemporaryUpload>` field marked `#[spark(upload(...))]`.
///
/// ```
/// # use smeltery::prelude::*;
/// # use serde::{Deserialize, Serialize};
/// #[derive(Serialize, Deserialize, Default, Spark, Validate)]
/// pub struct Avatar {
///     #[spark(upload(max = 2048, mimes = "png,jpg,jpeg"))]
///     #[validate(required)]
///     pub photo: Option<TemporaryUpload>,
///     pub path: Option<String>,
/// }
///
/// #[actions]
/// impl Avatar {
///     pub async fn save(&mut self, ctx: &mut SparkCtx) -> Result<()> {
///         ctx.validate(self).await?;
///         if let Some(photo) = self.photo.take() {
///             self.path = Some(photo.store(ctx, "public/avatars").await?);
///         }
///         Ok(())
///     }
/// }
/// # fn main() {}
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporaryUpload {
    id: String,
    name: String,
    mime: String,
    size: u64,
}

impl TemporaryUpload {
    /// The file name the browser sent (without directories).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The content type the browser sent.
    pub fn mime(&self) -> &str {
        &self.mime
    }

    /// The size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The file name's extension, lowercase.
    pub fn extension(&self) -> Option<String> {
        extension(&self.name)
    }

    /// Where the temp file is.
    ///
    /// # Errors
    /// The id is not a temp id (only the server creates them).
    pub fn temp_path(&self, ctx: &SparkCtx) -> Result<PathBuf> {
        if self.id.len() != 40 || !self.id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(Error::bad_request("not a temporary upload"));
        }
        Ok(tmp_dir(ctx.app()).join(&self.id))
    }

    /// The file's bytes.
    ///
    /// # Errors
    /// The temp file is gone (stored already, or cleaned up after 24 hours).
    pub async fn bytes(&self, ctx: &SparkCtx) -> Result<Vec<u8>> {
        Ok(tokio::fs::read(self.temp_path(ctx)?).await?)
    }

    /// Move the file into `storage/app/<dir>` under a random name with the file's extension; returns the path
    /// relative to `storage/app`, such as `public/avatars/<40 characters>.png`. `dir` starts with `public` (served
    /// at `/storage/…` through `smeltery storage:link`) or `private`. The extension is kept only when it is on core's
    /// list of safe extensions (`smeltery_core::http::is_safe_extension`: images, audio, video, PDF, text and
    /// tables, office documents, archives); any other one (`html`, `svg`, `xml`, `js`, an unknown one …) becomes
    /// `.bin`.
    ///
    /// # Errors
    /// An invalid `dir` (see [`TemporaryUpload::store_as`]), a missing temp file, or an I/O error.
    pub async fn store(&self, ctx: &SparkCtx, dir: &str) -> Result<String> {
        let mut file = random_token(40)?;
        if let Some(ext) = self.extension() {
            file.push('.');
            // Only an allow-listed extension stays (the same rule as core's `UploadedFile::store`), so a file served
            // from `/storage` can never script the site.
            file.push_str(&smeltery_core::http::stored_extension(&ext));
        }
        self.store_as(ctx, dir, &file).await
    }

    /// Move the file to `storage/app/<dir>/<file_name>`; returns the path relative to `storage/app`.
    ///
    /// `dir` starts with `public` or `private`; each segment of `dir` and `file_name` uses only
    /// `[A-Za-z0-9._-]`, is not `.` or `..`, and `file_name` does not start with a dot.
    ///
    /// # Errors
    /// An invalid `dir` or `file_name` (nothing moves), a missing temp file, or an I/O error.
    pub async fn store_as(&self, ctx: &SparkCtx, dir: &str, file_name: &str) -> Result<String> {
        let segments = checked_dir(dir)?;
        if !safe_segment(file_name) || file_name.starts_with('.') {
            return Err(Error::bad_request(format!(
                "invalid file name `{file_name}`"
            )));
        }
        let from = self.temp_path(ctx)?;
        let mut target = ctx.app().settings().storage_dir().join("app");
        for s in &segments {
            target.push(s);
        }
        create_dirs(&target).await?;
        target.push(file_name);
        if tokio::fs::rename(&from, &target).await.is_err() {
            // Another file system: copy, then remove the temp file.
            tokio::fs::copy(&from, &target).await?;
            tokio::fs::remove_file(&from).await?;
        }
        let mut rel = segments.join("/");
        rel.push('/');
        rel.push_str(file_name);
        Ok(rel)
    }
}

/// `#[validate(...)]` on an upload field: `required` passes once a file is chosen, `min`, `max` and
/// `between` count kilobytes, and `mimes = "png,jpg"` checks the name's extension.
impl smeltery_core::validation::rules::AsSubject for TemporaryUpload {
    fn as_subject(&self) -> smeltery_core::validation::rules::Subject<'_> {
        smeltery_core::validation::rules::Subject::Upload {
            size: self.size,
            name: &self.name,
            mime: &self.mime,
        }
    }
}

/// A file name's extension, lowercase (`a.b.PNG` → `png`).
pub(crate) fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty()
        && !ext.is_empty()
        && ext.len() <= 16
        && ext.bytes().all(|b| b.is_ascii_alphanumeric()))
    .then(|| ext.to_ascii_lowercase())
}

fn safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.len() <= 255
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `public/avatars` → `["public", "avatars"]`; anything outside `public` / `private` or with odd segments fails.
fn checked_dir(dir: &str) -> Result<Vec<&str>> {
    let segments: Vec<&str> = dir.trim_end_matches('/').split('/').collect();
    let root_ok = matches!(segments.first(), Some(&"public" | &"private"));
    if !root_ok || !segments.iter().all(|s| safe_segment(s)) {
        return Err(Error::bad_request(format!(
            "invalid storage directory `{dir}`: use `public/…` or `private/…`"
        )));
    }
    Ok(segments)
}

/// Framework scratch lives under `storage/framework`, so `storage/app` holds only user files.
fn tmp_dir(app: &App) -> PathBuf {
    app.settings()
        .storage_dir()
        .join("framework")
        .join("sparks")
}

/// `len` random characters from `[A-Za-z0-9]` (OS random source, rejection sampling).
pub(crate) fn random_token(len: usize) -> Result<String> {
    const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 64];
    while out.len() < len {
        getrandom::fill(&mut buf).map_err(|e| Error::internal(format!("no random source: {e}")))?;
        for b in buf {
            if b < 248
                && let Some(c) = ALPHABET.get(usize::from(b % 62))
            {
                out.push(char::from(*c));
                if out.len() == len {
                    break;
                }
            }
        }
    }
    Ok(out)
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What an upload token holds.
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    /// Component name.
    c: String,
    /// Field.
    f: String,
    /// Temp id (absent when a rule failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    /// File name, type, size.
    n: String,
    m: String,
    s: u64,
    /// Expiry, Unix seconds.
    e: u64,
    /// Hash of the session's CSRF token.
    b: String,
    /// The failed rule's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    err: Option<String>,
}

/// A hash of the session's CSRF secret ([`Session::binding`]): binds upload tokens and snapshots to the session.
pub(crate) fn binding(session: &Session) -> String {
    session.binding()
}

fn issue(app: &App, claims: &Claims) -> Result<String> {
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims)?);
    let signature = app.sign(PURPOSE, body.as_bytes())?;
    Ok(format!("{body}.{signature}"))
}

fn forbidden(component: &str, reason: &'static str) -> Error {
    tracing::warn!(component, reason, "Sparks upload token rejected");
    Error::http(StatusCode::FORBIDDEN, "Invalid upload token")
}

/// What an upload field's update becomes.
pub(crate) enum Resolved {
    /// The field's new value (a `TemporaryUpload` object or `null`).
    Value(serde_json::Value),
    /// A validation message for the field.
    Error(String),
}

/// Check an upload token sent as a field update.
pub(crate) fn resolve(
    app: &App,
    session: &Session,
    component: &str,
    rule: &UploadRule,
    value: serde_json::Value,
) -> Result<Resolved> {
    let token = match value {
        serde_json::Value::Null => return Ok(Resolved::Value(serde_json::Value::Null)),
        serde_json::Value::String(token) => token,
        _ => return Err(forbidden(component, "not a token")),
    };
    let Some((body, signature)) = token.split_once('.') else {
        return Err(forbidden(component, "malformed"));
    };
    if !app.verify_signature(PURPOSE, body.as_bytes(), signature) {
        return Err(forbidden(component, "bad signature"));
    }
    let claims: Claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| forbidden(component, "malformed"))?;
    if claims.c != component || claims.f != rule.field() {
        return Err(forbidden(component, "another component or field"));
    }
    if !same_secret(&claims.b, &binding(session)) {
        return Err(forbidden(component, "another session"));
    }
    if let Some(message) = claims.err {
        return Ok(Resolved::Error(message));
    }
    if now_secs() >= claims.e {
        return Ok(Resolved::Error(format!(
            "The {} upload has expired.",
            label(rule.field())
        )));
    }
    let Some(id) = claims.id else {
        return Err(forbidden(component, "no file"));
    };
    let upload = TemporaryUpload {
        id,
        name: claims.n,
        mime: claims.m,
        size: claims.s,
    };
    Ok(Resolved::Value(serde_json::to_value(upload)?))
}

pub(crate) fn same_secret(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq as _;
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// The query of `POST /_sparks/upload`.
#[derive(Debug, Deserialize)]
pub(crate) struct UploadQuery {
    component: String,
    field: String,
    #[serde(default)]
    name: String,
}

/// `POST /_sparks/upload?component=…&field=…&name=…` with the raw file as the body: `{"token": "…"}`.
pub(crate) async fn upload(
    app: App,
    session: Session,
    client: ClientInfo,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match receive(&app, &session, client.ip(), &query, &headers, body).await {
        Ok(token) => axum::Json(serde_json::json!({ "token": token })).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn receive(
    app: &App,
    session: &Session,
    ip: Option<std::net::IpAddr>,
    query: &UploadQuery,
    headers: &HeaderMap,
    body: Body,
) -> Result<String> {
    let runtime = Runtime::of(app)?;
    let rule = runtime
        .entry(query.component.as_str())
        .and_then(|e| e.uploads().iter().find(|u| u.field() == query.field))
        .copied()
        .ok_or_else(|| {
            tracing::warn!(component = %query.component, field = %query.field, "Sparks upload rejected: not an upload field");
            Error::http(StatusCode::FORBIDDEN, "Not an upload field")
        })?;
    cleanup(app, &runtime).await;
    let quota_keys = quota_keys(session, ip);
    runtime.reserve_upload(&quota_keys)?;

    // Only the last path segment of the name, at most 200 characters.
    let name: String = query
        .name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let name = if name.is_empty() {
        "file".to_owned()
    } else {
        name
    };
    let mime: String = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .chars()
        .take(100)
        .collect();
    let mut claims = Claims {
        c: query.component.clone(),
        f: query.field.clone(),
        id: None,
        n: name,
        m: mime,
        s: 0,
        e: now_secs().saturating_add(runtime.upload_ttl.as_secs()),
        b: binding(session),
        err: None,
    };
    let field_label = label(rule.field());
    if !rule.allows(&claims.n) {
        claims.err = Some(format!(
            "The {field_label} must be a file of type: {}.",
            rule.mimes().join(", ")
        ));
        return issue(app, &claims);
    }

    let dir = tmp_dir(app);
    create_dirs(&dir).await?;
    let id = random_token(40)?;
    let path = dir.join(&id);
    let max = rule.max_kb().saturating_mul(1024);
    // The bytes count against the quotas as they arrive, and the temp file goes when the upload does not finish
    // (an error, too large, or the request cancelled by its timeout or a dropped connection).
    let written = write_limited(&path, body, max, |bytes| {
        runtime.count_upload_bytes(&quota_keys, bytes);
    })
    .await?;
    match written {
        (mut temp, Some(size)) => {
            temp.keep = true;
            claims.id = Some(id);
            claims.s = size;
        }
        (_removed, None) => {
            claims.err = Some(format!(
                "The {field_label} must not be greater than {} kilobytes.",
                rule.max_kb()
            ));
        }
    }
    issue(app, &claims)
}

/// One session's upload use in the current quota window.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Use {
    /// When the window started.
    since: std::time::Instant,
    files: u32,
    bytes: u64,
}

/// Above this many tracked sessions, windows that ended are dropped before a new one is added.
const QUOTA_PRUNE_AT: usize = 4096;

/// The quota counters an upload counts against: the session's (its binding) and, when known, the client
/// address's (`ip:<address>`; the binding is hex, so the keys never collide).
fn quota_keys(session: &Session, ip: Option<std::net::IpAddr>) -> Vec<(String, bool)> {
    let mut keys = vec![(binding(session), false)];
    if let Some(ip) = ip {
        keys.push((format!("ip:{}", quota_ip(ip)), true));
    }
    keys
}

/// The client address as the upload quota counts it: an IPv6 client by its /64 (one customer's network holds 2^64
/// addresses), an IPv4 address (also when IPv4-mapped) as it is. The same folding as core's throttles, which keep
/// theirs crate-private.
pub(crate) fn quota_ip(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let [a, b, c, d, ..] = v6.segments();
                format!("{a:x}:{b:x}:{c:x}:{d:x}::/64")
            }
        },
        std::net::IpAddr::V4(v4) => v4.to_string(),
    }
}

impl Runtime {
    /// Count an upload request against each of `keys` (`(key, is_address)`); 429, counting nothing, when one of
    /// them used its files or bytes for this window.
    pub(crate) fn reserve_upload(&self, keys: &[(String, bool)]) -> Result<()> {
        let limits = self.limits;
        let now = std::time::Instant::now();
        let mut uses = self
            .uploads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if uses.len() >= QUOTA_PRUNE_AT {
            uses.retain(|_, u| now.duration_since(u.since) < limits.upload_window);
        }
        for (key, address) in keys {
            let (max_files, max_bytes) = if *address {
                (limits.upload_address_files, limits.upload_address_bytes)
            } else {
                (limits.upload_files, limits.upload_bytes)
            };
            let fresh = Use {
                since: now,
                files: 0,
                bytes: 0,
            };
            let entry = uses.get(key).copied().unwrap_or(fresh);
            let entry = if now.duration_since(entry.since) >= limits.upload_window {
                fresh
            } else {
                entry
            };
            if entry.files >= max_files || entry.bytes >= max_bytes {
                let whose = if *address {
                    "client address"
                } else {
                    "session"
                };
                tracing::warn!(whose, "Sparks upload rejected: the upload quota is used up");
                return Err(Error::http(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many uploads: try again later",
                ));
            }
            uses.insert(key.clone(), entry);
        }
        for (key, _) in keys {
            if let Some(entry) = uses.get_mut(key) {
                entry.files += 1;
            }
        }
        Ok(())
    }

    /// Add `bytes` stored by an upload to each counter of `keys`.
    pub(crate) fn count_upload_bytes(&self, keys: &[(String, bool)], bytes: u64) {
        let mut uses = self
            .uploads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (key, _) in keys {
            if let Some(entry) = uses.get_mut(key) {
                entry.bytes = entry.bytes.saturating_add(bytes);
            }
        }
    }
}

/// Stream `body` into `path`; `None` when it is larger than `max` bytes (reading stops there).
/// Unix permission bits of the upload folders Sparks creates: owner and group only.
#[cfg(unix)]
const DIR_MODE: u32 = 0o750;
/// Unix permission bits of the temp upload files (kept by `store`): owner and group only.
#[cfg(unix)]
const FILE_MODE: u32 = 0o640;

/// Create `dir` and its missing parents, never readable by every local user on Unix.
async fn create_dirs(dir: &Path) -> std::io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(DIR_MODE);
    builder.create(dir).await
}

/// A temp upload file, removed when dropped unless `keep` is set: an upload that does not finish (an error, too
/// large, or the request future dropped by the request timeout or a closed connection) leaves nothing behind.
#[derive(Debug)]
pub(crate) struct TempFile {
    path: PathBuf,
    pub(crate) keep: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Write `body` to the new file `path` (never an existing one), at most `max` bytes, calling `received` with the
/// size of each chunk as it arrives: the file guard and its size, or `None` (and the guard) when it was too large.
async fn write_limited(
    path: &Path,
    body: Body,
    max: u64,
    mut received: impl FnMut(u64),
) -> Result<(TempFile, Option<u64>)> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(FILE_MODE);
    let mut file = options.open(path).await?;
    let temp = TempFile {
        path: path.to_path_buf(),
        keep: false,
    };
    let mut stream = body.into_data_stream();
    let mut size = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::bad_request("the upload was interrupted"))?;
        let len = chunk.len() as u64;
        received(len);
        size = size.saturating_add(len);
        if size > max {
            return Ok((temp, None));
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    Ok((temp, Some(size)))
}

/// Delete temp files older than 24 hours, at most once every 10 minutes.
async fn cleanup(app: &App, runtime: &Runtime) {
    let now = now_secs();
    let last = runtime.last_cleanup.load(Ordering::Relaxed);
    if now.saturating_sub(last) < CLEANUP_EVERY
        || runtime
            .last_cleanup
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    let dir = tmp_dir(app);
    let removed = tokio::task::spawn_blocking(move || remove_old(&dir, TMP_MAX_AGE)).await;
    if let Ok(n) = removed
        && n > 0
    {
        tracing::debug!(files = n, "removed old Sparks temp uploads");
    }
}

/// Remove the files in `dir` older than `age`; returns how many.
pub(crate) fn remove_old(dir: &Path, age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|d| d >= age);
        if old
            && entry.file_type().is_ok_and(|t| t.is_file())
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_and_segments() {
        assert_eq!(extension("a.b.PNG").as_deref(), Some("png"));
        assert_eq!(extension(".env"), None);
        assert_eq!(extension("noext"), None);
        assert_eq!(extension("x.p/g"), None);
        assert!(checked_dir("public/avatars").is_ok());
        assert!(checked_dir("private").is_ok());
        assert!(checked_dir("public/../private").is_err());
        assert!(checked_dir("../public").is_err());
        assert!(checked_dir("/etc").is_err());
        assert!(checked_dir("other/x").is_err());
        assert!(checked_dir("public//x").is_err());
        assert!(checked_dir("public/a b").is_err());
        assert!(!safe_segment(".."));
        assert_eq!(random_token(40).unwrap().len(), 40);
    }

    /// Sweep W6-06: temp uploads and the folders Sparks creates are never readable by every local user.
    #[cfg(unix)]
    #[tokio::test]
    async fn upload_files_and_folders_are_not_world_readable() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("framework/sparks");
        create_dirs(&folder).await.unwrap();
        let path = folder.join("x");
        let (mut temp, size) = write_limited(&path, Body::from("abc"), 10, |_| {})
            .await
            .unwrap();
        temp.keep = true;
        assert_eq!(size, Some(3));
        for p in [dir.path().join("framework"), folder, path] {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o007, 0, "{} is {mode:o}", p.display());
        }
    }

    /// Sweep W6-01: a temp file is never an existing file, and goes when its upload does not finish.
    #[tokio::test]
    async fn temp_files_are_new_and_removed_unless_kept() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("taken");
        std::fs::write(&existing, b"x").unwrap();
        assert!(
            write_limited(&existing, Body::from("abc"), 10, |_| {})
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(&existing).unwrap(),
            b"x",
            "an existing file is never written"
        );
        let big = dir.path().join("big");
        let (temp, size) = write_limited(&big, Body::from("abcdef"), 3, |_| {})
            .await
            .unwrap();
        assert_eq!(size, None);
        drop(temp);
        assert!(!big.exists(), "a too large upload leaves nothing");
    }

    #[test]
    fn remove_old_keeps_new_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("new"), b"x").unwrap();
        let old = dir.path().join("old");
        std::fs::write(&old, b"x").unwrap();
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(25 * 60 * 60))
            .unwrap();
        drop(f);
        assert_eq!(remove_old(dir.path(), TMP_MAX_AGE), 1);
        assert!(dir.path().join("new").exists());
        assert!(!old.exists());
        assert_eq!(remove_old(&dir.path().join("missing"), TMP_MAX_AGE), 0);
    }
}
