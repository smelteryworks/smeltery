//! Files uploaded with a `multipart/form-data` form: [`UploadedFile`], and reading such a form
//! for [`Valid`](crate::validation::Valid) (D-130).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use http::StatusCode;
use serde::de::{self, Deserialize, Deserializer, Visitor};
use tokio::io::AsyncWriteExt as _;

use crate::app::App;
use crate::error::{Error, Result};
use crate::validation::rules::{self, AsSubject, Subject};
use crate::validation::{Input, Invalid, ValidationErrors};

/// How many leading bytes the content sniffing looks at.
const SNIFF_LEN: usize = 16;
/// Temp files older than this are deleted (a crashed request can leave one behind).
const TMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// How often (seconds) a multipart request looks for old temp files.
const SWEEP_EVERY: u64 = 10 * 60;
static LAST_SWEEP: AtomicU64 = AtomicU64::new(0);
/// What a multipart body may hold besides field data (boundaries, part headers, a preamble)
/// on top of `UPLOAD_MAX_BYTES`.
const MULTIPART_OVERHEAD: u64 = 1024 * 1024;
/// The most parts (fields and files) one multipart body may have.
pub(crate) const MAX_PARTS: usize = 1000;
/// What a file field holds while the form deserializes: this prefix and a random key.
const KEY_PREFIX: &str = "\u{1}smeltery-upload:";

/// A file uploaded with a `multipart/form-data` form, as a field of a [`Valid`] form struct.
///
/// The file waits in `storage/framework/uploads/` (never under `public/`) until
/// [`store`](Self::store) or [`store_as`](Self::store_as) moves it; a file that is not stored is
/// deleted when the value is dropped, at the end of the request at the latest.
///
/// ```
/// # use serde::Deserialize;
/// # use smeltery::http::UploadedFile;
/// # use smeltery::prelude::*;
/// #[derive(Deserialize, Validate)]
/// pub struct PhotoForm {
///     #[validate(required, max = 255)]
///     pub title: String,
///     #[validate(required, max = 2048, mimes = "png,jpg")]
///     pub image: Option<UploadedFile>,
/// }
///
/// async fn store(Valid(form): Valid<PhotoForm>) -> Result<Redirect> {
///     if let Some(image) = &form.image {
///         let path = image.store("public/photos").await?; // "public/photos/<random>.png"
///     }
///     Ok(Redirect::to("/photos"))
/// }
/// # fn main() {}
/// ```
///
/// [`Valid`]: crate::validation::Valid
pub struct UploadedFile {
    name: String,
    mime: String,
    size: u64,
    temp: PathBuf,
    /// `<root>/storage/app`.
    storage: PathBuf,
}

impl std::fmt::Debug for UploadedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadedFile")
            .field("name", &self.name)
            .field("mime", &self.mime)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl UploadedFile {
    /// The file name the browser sent, without directories and with unsafe characters
    /// replaced (`../../etc/passwd` â†’ `passwd`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The content type: read from the first bytes for PNG, JPEG, GIF, WebP and PDF files,
    /// else the one the browser sent (`application/octet-stream` when none).
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
    pub fn temp_path(&self) -> &Path {
        &self.temp
    }

    /// The file's bytes.
    ///
    /// # Errors
    /// The temp file is gone (the file was stored already) or cannot be read.
    pub async fn bytes(&self) -> Result<Vec<u8>> {
        Ok(tokio::fs::read(&self.temp).await?)
    }

    /// Move the file into `storage/app/<dir>` under a random name; returns the path relative
    /// to `storage/app`, such as `public/photos/<40 characters>.png`. `dir` starts with
    /// `public` (served at `/storage/â€¦` through `smeltery storage:link`) or `private`.
    ///
    /// The extension follows the content for PNG, JPEG, GIF, WebP and PDF files (`png`, `jpg`,
    /// `gif`, `webp`, `pdf`; a `jpeg` name keeps `jpeg`). Any other file keeps the extension of
    /// its name when it is on the list of safe extensions ([`is_safe_extension`]: images,
    /// audio, video, text and tables, office documents, archives); every other extension
    /// (`html`, `svg`, `xml`, `js`, an unknown one …) is stored as `.bin`.
    ///
    /// # Errors
    /// An invalid `dir` (see [`store_as`](Self::store_as)), a file stored already, or an I/O
    /// error.
    pub async fn store(&self, dir: &str) -> Result<String> {
        let mut file = crate::crypto::random_token(40)?;
        if let Some(ext) = self.stored_extension() {
            file.push('.');
            file.push_str(&ext);
        }
        self.store_as(dir, &file).await
    }

    /// The extension [`store`](Self::store) gives the file.
    fn stored_extension(&self) -> Option<String> {
        let named = self.extension();
        if let Some((_, extensions)) = SNIFFED.iter().find(|(mime, _)| *mime == self.mime) {
            // The content was read as this type (`content_type` refuses a mismatch).
            return match named {
                Some(ext) if extensions.contains(&ext.as_str()) => Some(ext),
                _ => extensions.first().map(|ext| (*ext).to_owned()),
            };
        }
        named.map(|ext| stored_extension(&ext))
    }

    /// Move the file to `storage/app/<dir>/<file_name>`; returns the path relative to
    /// `storage/app`.
    ///
    /// `dir` starts with `public` or `private`; each segment of `dir` and `file_name` uses only
    /// `[A-Za-z0-9._-]`, is not `.` or `..`, and `file_name` does not start with a dot.
    ///
    /// # Errors
    /// An invalid `dir` or `file_name` (nothing moves), a file stored already, or an I/O error.
    pub async fn store_as(&self, dir: &str, file_name: &str) -> Result<String> {
        let segments = checked_dir(dir)?;
        if !safe_segment(file_name) || file_name.starts_with('.') {
            return Err(Error::bad_request(format!(
                "invalid file name `{file_name}`"
            )));
        }
        let mut target = self.storage.clone();
        for s in &segments {
            target.push(s);
        }
        create_dirs(&target).await?;
        target.push(file_name);
        if tokio::fs::rename(&self.temp, &target).await.is_err() {
            // Another file system: copy, then remove the temp file.
            tokio::fs::copy(&self.temp, &target).await?;
            tokio::fs::remove_file(&self.temp).await?;
        }
        let mut rel = segments.join("/");
        rel.push('/');
        rel.push_str(file_name);
        Ok(rel)
    }
}

impl Drop for UploadedFile {
    fn drop(&mut self) {
        // One unlink in the temp directory: cheap enough to do in place, and spawning it would
        // leave a task without an owner. A stored file is gone already (NotFound is fine).
        let _ = std::fs::remove_file(&self.temp);
    }
}

/// `#[validate(...)]` on a file field: `required` passes when a file was sent, `min`, `max` and
/// `between` count kilobytes, and `mimes = "png,jpg"` checks the extension.
impl AsSubject for UploadedFile {
    fn as_subject(&self) -> Subject<'_> {
        Subject::Upload {
            size: self.size,
            name: &self.name,
            mime: &self.mime,
        }
    }
}

thread_local! {
    /// The files of the multipart form being deserialized on this thread, by key.
    static FILES: RefCell<HashMap<String, UploadedFile>> = RefCell::new(HashMap::new());
}

/// Run `f` (a synchronous deserialization) with `files` claimable by [`UploadedFile`]'s
/// `Deserialize`; files nobody claimed are dropped (deleted) afterwards.
pub(crate) fn with_files<R>(files: HashMap<String, UploadedFile>, f: impl FnOnce() -> R) -> R {
    FILES.with(|cell| *cell.borrow_mut() = files);
    let out = f();
    let unclaimed = FILES.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    drop(unclaimed);
    out
}

/// A fresh key for a file field.
pub(crate) fn file_key() -> Result<String> {
    Ok(format!("{KEY_PREFIX}{}", crate::crypto::random_token(24)?))
}

/// Deserializes only inside [`Valid`](crate::validation::Valid) reading a multipart form; any
/// other input (a URL-encoded or JSON value) is "not an uploaded file".
impl<'de> Deserialize<'de> for UploadedFile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct FileVisitor;

        impl Visitor<'_> for FileVisitor {
            type Value = UploadedFile;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an uploaded file")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<UploadedFile, E> {
                v.starts_with(KEY_PREFIX)
                    .then(|| FILES.with(|cell| cell.borrow_mut().remove(v)))
                    .flatten()
                    .ok_or_else(|| E::custom("expected an uploaded file"))
            }
        }

        deserializer.deserialize_str(FileVisitor)
    }
}

/// A multipart form as read: the text fields (in order) and the files by field.
#[derive(Debug, Default)]
pub(crate) struct MultipartInput {
    pub(crate) text: Vec<(String, String)>,
    pub(crate) files: Vec<(String, UploadedFile)>,
    /// Files refused while reading (content that does not match the type).
    pub(crate) file_errors: ValidationErrors,
}

/// Read a multipart body: text fields into memory (together at most `BODY_LIMIT`), files
/// streamed to temp files; the whole body at most `UPLOAD_MAX_BYTES`.
///
/// # Errors
/// A body over `UPLOAD_MAX_BYTES` (a validation error on the field being read), text fields
/// over `BODY_LIMIT` (413), a malformed body (400), or an I/O error.
pub(crate) async fn read(app: &App, body: Body, boundary: String) -> Result<MultipartInput> {
    let settings = app.settings();
    let max_total = settings.upload_max_bytes;
    let text_limit = u64::try_from(settings.body_limit).unwrap_or(u64::MAX);
    let storage = settings.storage_dir().join("app");
    // Framework scratch lives under `storage/framework`, so `storage/app` holds only user files.
    let tmp = settings.storage_dir().join("framework").join("uploads");
    sweep(&tmp).await;

    // The whole body, boundaries and part headers included, is bounded too: the field data
    // counted below never sees a preamble or a part header that does not end.
    let constraints = multer::Constraints::new().size_limit(
        multer::SizeLimit::new().whole_stream(max_total.saturating_add(MULTIPART_OVERHEAD)),
    );
    let mut multipart =
        multer::Multipart::with_constraints(body.into_data_stream(), boundary, constraints);
    let mut out = MultipartInput::default();
    let mut total: u64 = 0;
    let mut text_total: u64 = 0;
    let mut parts: usize = 0;
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) => return Err(malformed(&e)),
        };
        parts += 1;
        if parts > MAX_PARTS {
            return Err(Error::http(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload Too Large",
            ));
        }
        let Some(name) = field.name().map(str::to_owned) else {
            continue;
        };
        let Some(raw_name) = field.file_name().map(str::to_owned) else {
            let mut value = Vec::new();
            loop {
                match field.chunk().await {
                    Ok(Some(chunk)) => {
                        let len = chunk.len() as u64;
                        total = total.saturating_add(len);
                        text_total = text_total.saturating_add(len);
                        if text_total > text_limit {
                            return Err(Error::http(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "Payload Too Large",
                            ));
                        }
                        if total > max_total {
                            return Err(too_large(app, &name, max_total, &out));
                        }
                        value.extend_from_slice(&chunk);
                    }
                    Ok(None) => break,
                    Err(e) => return Err(malformed(&e)),
                }
            }
            let value = String::from_utf8(value)
                .map_err(|_| Error::bad_request("A form field is not valid UTF-8."))?;
            out.text.push((name, value));
            continue;
        };
        let declared = field
            .content_type()
            .map(|m| m.essence_str().to_owned())
            .unwrap_or_default();
        let mut file: Option<(tokio::fs::File, UploadedFile)> = None;
        let mut head: Vec<u8> = Vec::with_capacity(SNIFF_LEN);
        let mut size: u64 = 0;
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    let len = chunk.len() as u64;
                    size = size.saturating_add(len);
                    total = total.saturating_add(len);
                    if total > max_total {
                        // Dropping `file` removes the partial temp file.
                        return Err(too_large(app, &name, max_total, &out));
                    }
                    let room = SNIFF_LEN.saturating_sub(head.len()).min(chunk.len());
                    head.extend_from_slice(chunk.get(..room).unwrap_or_default());
                    if file.is_none() {
                        file = Some(create_temp(&tmp, &storage).await?);
                    }
                    if let Some((handle, _)) = file.as_mut() {
                        handle.write_all(&chunk).await?;
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(malformed(&e)),
            }
        }
        if raw_name.is_empty() && size == 0 {
            // A file input left empty: the field is absent.
            continue;
        }
        let (mut handle, mut upload) = match file {
            Some(open) => open,
            None => create_temp(&tmp, &storage).await?,
        };
        handle.flush().await?;
        drop(handle);
        upload.name = sanitize_name(&raw_name);
        upload.size = size;
        match content_type(&upload.name, &declared, &head) {
            Some(mime) => {
                upload.mime = mime;
                out.files.push((name, upload));
            }
            None => {
                let label = rules::label(&name);
                out.file_errors.add(
                    name,
                    format!("The {label} field must be a file whose content matches its type."),
                );
            }
        }
    }
    Ok(out)
}

fn malformed(e: &multer::Error) -> Error {
    if matches!(e, multer::Error::StreamSizeExceeded { .. }) {
        return Error::http(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large");
    }
    tracing::debug!(error = %e, "invalid multipart body");
    Error::bad_request("The multipart body is invalid.")
}

/// The validation failure for a body over `UPLOAD_MAX_BYTES`, on the field being read.
fn too_large(app: &App, field: &str, max_total: u64, read: &MultipartInput) -> Error {
    let mut errors = ValidationErrors::new();
    errors.add(
        field,
        format!(
            "The {} field must not be greater than {} kilobytes.",
            rules::label(field),
            max_total / 1024
        ),
    );
    let input: Input = read
        .text
        .iter()
        .filter(|(k, _)| k != "_token" && k != "_method")
        .cloned()
        .collect();
    Error::Validation(Box::new(Invalid::new(
        errors,
        crate::validation::old_input(app, &input),
    )))
}

/// Unix mode of upload files (temp files, and so the stored files they are renamed or copied to):
/// readable by the app's user and group only, like the log.
#[cfg(unix)]
const FILE_MODE: u32 = 0o640;

/// Unix mode of the folders uploads create (`storage/framework/uploads`, `storage/app/<dir>`).
#[cfg(unix)]
const DIR_MODE: u32 = 0o750;

/// Create `dir` and its missing parents; on Unix new folders get `0750` (minus the umask).
async fn create_dirs(dir: &Path) -> std::io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(DIR_MODE);
    builder.create(dir).await
}

async fn create_temp(tmp: &Path, storage: &Path) -> Result<(tokio::fs::File, UploadedFile)> {
    create_dirs(tmp).await?;
    let temp = tmp.join(crate::crypto::random_token(40)?);
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // Set at creation (never readable by every local user, whatever the umask lets through); a
    // rename keeps it and a copy across file systems copies it.
    #[cfg(unix)]
    options.mode(FILE_MODE);
    let handle = options.open(&temp).await?;
    let upload = UploadedFile {
        name: String::new(),
        mime: String::new(),
        size: 0,
        temp,
        storage: storage.to_path_buf(),
    };
    Ok((handle, upload))
}

/// Delete temp files older than a day, at most every ten minutes (on the blocking pool,
/// awaited, so no task outlives the request).
async fn sweep(tmp: &Path) {
    let now = now_secs();
    let last = LAST_SWEEP.load(Ordering::Relaxed);
    if now.saturating_sub(last) < SWEEP_EVERY
        || LAST_SWEEP
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    let dir = tmp.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || sweep_dir(&dir)).await;
}

fn sweep_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let ours = name.len() == 40
            && name
                .to_str()
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_alphanumeric()));
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > TMP_MAX_AGE);
        if ours && old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Images an uploaded file keeps the extension of (not SVG: it runs script).
const SAFE_IMAGES: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "gif", "webp", "avif", "bmp", "ico", "tif", "tiff",
    "heic", "heif",
];
/// Audio and video an uploaded file keeps the extension of.
const SAFE_MEDIA: &[&str] = &[
    "mp3", "wav", "ogg", "oga", "opus", "m4a", "aac", "flac", "weba", "mid", "midi", "mp4", "m4v",
    "webm", "ogv", "mov", "avi", "mkv", "mpeg", "mpg", "3gp",
];
/// Documents, tables and archives an uploaded file keeps the extension of.
const SAFE_DOCUMENTS: &[&str] = &[
    "pdf", "txt", "csv", "tsv", "rtf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods",
    "odp", "epub", "zip", "gz", "tgz", "7z", "rar", "tar", "bz2", "xz", "zst",
];
/// The extensions an uploaded file keeps when it is stored: images, audio, video, PDF, plain
/// text and tables, office documents and archives. No browser runs any of them as a page or a
/// script of the site; every other extension is stored as `.bin`.
const SAFE_EXTENSIONS: [&[&str]; 3] = [SAFE_IMAGES, SAFE_MEDIA, SAFE_DOCUMENTS];

/// Whether [`UploadedFile::store`] keeps the extension `ext` (any letter case, without the
/// dot): an image, audio, video, PDF, text or table, office document or archive type that no
/// browser runs as a page or script of the site. Any other extension (`html`, `svg`, `xml`,
/// `xsd`, `mathml`, `js`, an unknown one …) is stored as `.bin`; other code that names uploaded
/// files can use the same rule, or [`stored_extension`].
///
/// ```
/// use smeltery_core::http::is_safe_extension;
///
/// assert!(is_safe_extension("PNG") && is_safe_extension("pdf"));
/// assert!(!is_safe_extension("html") && !is_safe_extension("xsd") && !is_safe_extension("svg"));
/// ```
pub fn is_safe_extension(ext: &str) -> bool {
    let ext = ext.to_ascii_lowercase();
    SAFE_EXTENSIONS
        .iter()
        .any(|list| list.contains(&ext.as_str()))
}

/// The extension an uploaded file named with `ext` is stored with: `ext` in lowercase when
/// [`is_safe_extension`], else `bin`.
///
/// ```
/// use smeltery_core::http::stored_extension;
///
/// assert_eq!(stored_extension("JPG"), "jpg");
/// assert_eq!(stored_extension("xhtml"), "bin");
/// ```
pub fn stored_extension(ext: &str) -> String {
    if is_safe_extension(ext) {
        ext.to_ascii_lowercase()
    } else {
        "bin".to_owned()
    }
}

/// The types recognised from their first bytes, with their extensions.
const SNIFFED: &[(&str, &[&str])] = &[
    ("image/png", &["png"]),
    ("image/jpeg", &["jpg", "jpeg", "jpe", "jfif"]),
    ("image/gif", &["gif"]),
    ("image/webp", &["webp"]),
    ("application/pdf", &["pdf"]),
];

/// The type the first bytes show, for the types in [`SNIFFED`].
pub(crate) fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("image/webp")
    } else if head.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

/// A content type in lowercase without parameters, with common aliases folded.
fn normalize_mime(mime: &str) -> String {
    let essence = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "image/jpg" | "image/pjpeg" => "image/jpeg".to_owned(),
        "image/x-png" => "image/png".to_owned(),
        _ => essence,
    }
}

/// The type of a recognised extension.
fn extension_mime(ext: &str) -> Option<&'static str> {
    SNIFFED
        .iter()
        .find(|(_, exts)| exts.contains(&ext))
        .map(|(mime, _)| *mime)
}

/// The file's content type, or `None` when the content contradicts the declared type or the
/// extension: a recognised type whose bytes show another type, or bytes of a recognised type
/// declared or named as something else.
pub(crate) fn content_type(name: &str, declared: &str, head: &[u8]) -> Option<String> {
    let declared = normalize_mime(declared);
    let declared_known = !declared.is_empty() && declared != "application/octet-stream";
    let by_extension = extension(name).as_deref().and_then(extension_mime);
    match sniff(head) {
        Some(sniffed) => {
            let contradicted = (declared_known && declared != sniffed)
                || by_extension.is_some_and(|e| e != sniffed);
            (!contradicted).then(|| sniffed.to_owned())
        }
        None => {
            let claims_known = SNIFFED.iter().any(|(m, _)| *m == declared);
            if claims_known || by_extension.is_some() {
                None
            } else if declared_known {
                Some(declared)
            } else {
                Some("application/octet-stream".to_owned())
            }
        }
    }
}

/// The browser's file name made safe to show and to take an extension from: the last path
/// segment, control and reserved characters replaced, no leading dots, at most 255 bytes.
pub(crate) fn sanitize_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned
        .trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' ']);
    let mut name = String::new();
    for c in trimmed.chars() {
        if name.len() + c.len_utf8() > 255 {
            break;
        }
        name.push(c);
    }
    if name.is_empty() {
        "file".to_owned()
    } else {
        name
    }
}

/// A file name's extension, lowercase (`a.b.PNG` â†’ `png`).
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

/// `public/photos` â†’ `["public", "photos"]`; anything outside `public` / `private` or with odd
/// segments fails.
fn checked_dir(dir: &str) -> Result<Vec<&str>> {
    let segments: Vec<&str> = dir.trim_end_matches('/').split('/').collect();
    let root_ok = matches!(segments.first(), Some(&"public" | &"private"));
    if !root_ok || !segments.iter().all(|s| safe_segment(s)) {
        return Err(Error::bad_request(format!(
            "invalid storage directory `{dir}`: use `public/â€¦` or `private/â€¦`"
        )));
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("C:\\Users\\a\\photo.PNG"), "photo.PNG");
        assert_eq!(sanitize_name("..hidden"), "hidden");
        assert_eq!(sanitize_name("a\0b<c>.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_name("../"), "file");
        assert_eq!(sanitize_name(""), "file");
        assert_eq!(sanitize_name(&"Ã©".repeat(300)).len(), 254);
    }

    #[test]
    fn content_must_match_the_type() {
        assert_eq!(
            content_type("a.png", "image/png", PNG).as_deref(),
            Some("image/png")
        );
        assert_eq!(content_type("a.dat", "", PNG).as_deref(), Some("image/png"));
        // PNG bytes named or declared as something else.
        assert_eq!(content_type("a.jpg", "image/jpeg", PNG), None);
        assert_eq!(content_type("a.png", "text/plain", PNG), None);
        // Text claiming to be an image.
        assert_eq!(content_type("a.png", "image/png", b"<?php echo 1;"), None);
        assert_eq!(content_type("a.txt", "image/png", b"hello"), None);
        assert_eq!(content_type("a.jpg", "", b"hello"), None);
        // Unknown types keep the declared one.
        assert_eq!(
            content_type("notes.txt", "text/plain; charset=utf-8", b"hello").as_deref(),
            Some("text/plain")
        );
        assert_eq!(
            content_type("blob", "", b"hello").as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(
            content_type("a.jpeg", "image/jpg", b"\xff\xd8\xff\xe0").as_deref(),
            Some("image/jpeg")
        );
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"GIF89a"), Some("image/gif"));
        assert_eq!(sniff(b"%PDF-1.7"), Some("application/pdf"));
    }

    #[test]
    fn store_paths_are_checked() {
        assert!(checked_dir("public/photos").is_ok());
        assert!(checked_dir("private").is_ok());
        assert!(checked_dir("public/../../x").is_err());
        assert!(checked_dir("../public").is_err());
        assert!(checked_dir("/etc").is_err());
        assert!(checked_dir("photos").is_err());
        assert!(checked_dir("public/a b").is_err());
        assert!(!safe_segment(".."));
        assert_eq!(extension("a.b.PNG").as_deref(), Some("png"));
        assert_eq!(extension(".png"), None);
    }

    /// Upload files and the folders made for them are private to the app's user and group
    /// (W6-06): files `0640`, folders `0750` (minus what the umask removes), never readable by
    /// other users whatever the umask.
    #[cfg(unix)]
    #[tokio::test]
    async fn uploads_are_not_readable_by_other_users() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("framework").join("uploads");
        let storage = dir.path().join("app");
        let (mut handle, mut file) = create_temp(&tmp, &storage).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut handle, b"secret")
            .await
            .unwrap();
        drop(handle);
        file.name = "a.txt".into();
        file.mime = "text/plain".into();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(&file.temp) & 0o007,
            0,
            "temp file {:o}",
            mode(&file.temp)
        );
        assert_eq!(mode(&tmp) & 0o007, 0, "temp folder {:o}", mode(&tmp));
        let stored = file.store_as("private/docs", "a.txt").await.unwrap();
        let stored = storage.join(stored);
        assert_eq!(mode(&stored) & 0o007, 0, "stored file {:o}", mode(&stored));
        assert_eq!(mode(&stored) & !0o640, 0, "stored file {:o}", mode(&stored));
        let folder = storage.join("private").join("docs");
        assert_eq!(
            mode(&folder) & 0o007,
            0,
            "stored folder {:o}",
            mode(&folder)
        );
    }

    #[test]
    fn unclaimed_files_are_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("t");
        std::fs::write(&temp, b"x").unwrap();
        let file = UploadedFile {
            name: "a.txt".into(),
            mime: "text/plain".into(),
            size: 1,
            temp: temp.clone(),
            storage: dir.path().to_path_buf(),
        };
        let key = file_key().unwrap();
        let claimed: Option<UploadedFile> = with_files(HashMap::from([(key, file)]), || {
            serde_json::from_str::<UploadedFile>("\"\\u0001smeltery-upload:nope\"").ok()
        });
        assert!(claimed.is_none());
        assert!(!temp.exists());
    }
}
