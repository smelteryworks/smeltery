//! Logging: the console log on stderr and the optional log file (`LOG_FILE`).
//!
//! With `LOG_FILE` set, every event also goes to that file as one plain-text line (timestamp, level, target,
//! message and fields, no colour codes), appended. The file is written by one dedicated thread fed by a
//! bounded channel: a request never waits for the disk. When the writer falls behind and the channel is full,
//! lines are dropped and counted, and the writer logs how many it lost. When the file grows past
//! `LOG_MAX_BYTES` it is renamed to `<file>.1` (replacing an older backup) and a new file is started.
//!
//! [`run`](crate::run) sets this up from the app's [`Settings`]; tests and tools can build a file-only
//! subscriber with [`FileLog::open`] and [`file_subscriber`]:
//!
//! ```
//! use smeltery_core::logging::{FileLog, file_subscriber};
//!
//! let dir = tempfile::tempdir().unwrap();
//! let (file, guard) = FileLog::open(dir.path().join("logs/app.log"), 10 * 1024 * 1024).unwrap();
//! tracing::subscriber::with_default(file_subscriber(file, "info"), || {
//!     tracing::error!(order = 7, "payment failed");
//! });
//! drop(guard); // flushes the file
//! let text = std::fs::read_to_string(dir.path().join("logs/app.log")).unwrap();
//! assert!(text.contains("ERROR") && text.contains("payment failed") && text.contains("order=7"));
//! ```

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, IsTerminal as _, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::config::Settings;

/// How many lines may wait for the writer thread before new ones are dropped.
const CHANNEL_CAPACITY: usize = 8192;

/// How long dropping a [`LogGuard`] waits for the writer to flush.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// Install the global `tracing` subscriber for `settings`: the console log on stderr, plus the log file when
/// `LOG_FILE` is set. Keep the returned guard until the program ends: dropping it flushes the file.
///
/// A second call installs nothing (the first subscriber stays) and returns an empty guard. A log file that
/// cannot be opened is reported on the console log, and the app runs with stderr only.
pub fn init(settings: &Settings) -> LogGuard {
    let level = level_filter(&settings.log_level);
    let mut open_error = None;
    let (file, guard) = match &settings.log_file {
        Some(path) => match FileLog::open(path, settings.log_max_bytes) {
            Ok((file, guard)) => (Some(file), guard),
            Err(e) => {
                open_error = Some((path.clone(), e));
                (None, LogGuard::empty())
            }
        },
        None => (None, LogGuard::empty()),
    };
    let console = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(console_ansi())
        .with_writer(EscapedStderr);
    let file_layer = file.map(|f| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(f.writer)
    });
    let installed = tracing_subscriber::registry()
        .with(level)
        .with(console)
        .with(file_layer)
        .try_init()
        .is_ok();
    if let Some((path, error)) = open_error {
        tracing::warn!(path = %path.display(), %error, "cannot open LOG_FILE; logging to stderr only");
    }
    if installed { guard } else { LogGuard::empty() }
}

/// A subscriber writing only to `file`, at `level` (`trace` … `error`, else `info`). For tests and tools that
/// want the file format without the console log; [`init`] is what apps use.
pub fn file_subscriber(file: FileLog, level: &str) -> impl tracing::Subscriber + Send + Sync {
    tracing_subscriber::registry()
        .with(level_filter(level))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file.writer),
        )
}

/// Whether the console log uses colour codes. tracing-subscriber colours whenever `NO_COLOR` is unset, even
/// when stderr is redirected, which leaves escape codes in log files, `docker logs` and journald; so colour
/// only on a terminal.
pub(crate) fn console_ansi() -> bool {
    ansi_for(io::stderr().is_terminal(), std::env::var_os("NO_COLOR"))
}

fn ansi_for(is_terminal: bool, no_color: Option<OsString>) -> bool {
    is_terminal && no_color.is_none_or(|v| v.is_empty())
}

pub(crate) fn level_filter(level: &str) -> LevelFilter {
    match level.to_ascii_lowercase().as_str() {
        "trace" => LevelFilter::TRACE,
        "debug" => LevelFilter::DEBUG,
        "warn" => LevelFilter::WARN,
        "error" => LevelFilter::ERROR,
        _ => LevelFilter::INFO,
    }
}

/// One event's output with every line break inside it written as `\n` / `\r`, keeping only the final newline.
/// tracing-subscriber escapes control characters such as ESC but not line breaks
/// (`tracing-subscriber-0.3.23 src/fmt/format/escape.rs:22-43`), so a value from outside (a database error that
/// quotes user input, a cache key) could otherwise start a forged line of its own.
pub(crate) fn escape_line_breaks(event: &[u8]) -> Vec<u8> {
    let (body, end) = match event.strip_suffix(b"\n") {
        Some(body) => (body, &b"\n"[..]),
        None => (event, &b""[..]),
    };
    let mut out = Vec::with_capacity(event.len() + 8);
    for &b in body {
        match b {
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b => out.push(b),
        }
    }
    out.extend_from_slice(end);
    out
}

/// The console log's writer: stderr, one event at a time, line breaks inside an event escaped.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EscapedStderr;

impl<'a> MakeWriter<'a> for EscapedStderr {
    type Writer = EscapedEvent;

    fn make_writer(&'a self) -> Self::Writer {
        EscapedEvent { buf: Vec::new() }
    }
}

/// Collects one event, then writes it to stderr escaped (see [`escape_line_breaks`]).
pub(crate) struct EscapedEvent {
    buf: Vec<u8>,
}

impl Write for EscapedEvent {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for EscapedEvent {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            let _ = io::stderr()
                .lock()
                .write_all(&escape_line_breaks(&self.buf));
        }
    }
}

/// A log file: where the file layer sends its lines. Made with [`FileLog::open`]; used with
/// [`file_subscriber`] (or by [`init`]).
#[derive(Debug)]
pub struct FileLog {
    writer: ChannelWriter,
}

impl FileLog {
    /// Open (create, append) the log file at `path`, creating its parent directories, and start its writer
    /// thread. The file is rotated to `<path>.1` when a line would take it past `max_bytes`.
    ///
    /// # Errors
    /// The directory or the file cannot be created or opened.
    pub fn open(path: impl Into<PathBuf>, max_bytes: u64) -> io::Result<(Self, LogGuard)> {
        let file = RotatingFile::open(path.into(), max_bytes.max(1))?;
        Self::with_sink(Box::new(file), CHANNEL_CAPACITY)
    }

    /// The writer thread over any sink, with a channel of `capacity` lines.
    pub(crate) fn with_sink(
        sink: Box<dyn Write + Send>,
        capacity: usize,
    ) -> io::Result<(Self, LogGuard)> {
        let (tx, rx) = mpsc::sync_channel(capacity.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let lost = Arc::clone(&dropped);
        let handle = std::thread::Builder::new()
            .name("smeltery-log".into())
            .spawn(move || write_loop(&rx, sink, &lost))?;
        let writer = ChannelWriter {
            tx: tx.clone(),
            dropped: Arc::clone(&dropped),
        };
        let guard = LogGuard {
            inner: Some(GuardInner { tx, handle }),
            dropped,
        };
        Ok((Self { writer }, guard))
    }
}

/// Owns the log file's writer thread. Dropping it writes out every queued line (waiting at most two seconds)
/// and stops the thread; lines logged after that are not written.
#[derive(Debug)]
#[must_use = "dropping the guard stops the log file writer"]
pub struct LogGuard {
    inner: Option<GuardInner>,
    dropped: Arc<AtomicU64>,
}

#[derive(Debug)]
struct GuardInner {
    tx: SyncSender<Msg>,
    handle: JoinHandle<()>,
}

impl LogGuard {
    fn empty() -> Self {
        Self {
            inner: None,
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// How many lines were dropped so far because the writer fell behind.
    pub fn dropped_lines(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for LogGuard {
    fn drop(&mut self) {
        let Some(GuardInner { tx, handle }) = self.inner.take() else {
            return;
        };
        let deadline = Instant::now() + FLUSH_TIMEOUT;
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let mut msg = Msg::Stop(ack_tx);
        // Never block forever on a stuck disk: retry a full channel until the deadline.
        loop {
            match tx.try_send(msg) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return,
                Err(TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        return;
                    }
                    msg = back;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if ack_rx.recv_timeout(left).is_ok() {
            let _ = handle.join();
        }
    }
}

enum Msg {
    Line(Vec<u8>),
    Stop(SyncSender<()>),
}

/// The fmt layer's writer: hands each event's line to the writer thread without waiting.
#[derive(Clone, Debug)]
struct ChannelWriter {
    tx: SyncSender<Msg>,
    dropped: Arc<AtomicU64>,
}

impl<'a> MakeWriter<'a> for ChannelWriter {
    type Writer = LineWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LineWriter {
            buf: Vec::new(),
            log: self,
        }
    }
}

/// Collects one event's output; sends it as one line when dropped (the fmt layer writes one event per
/// writer, `tracing-subscriber/src/fmt/fmt_layer.rs` `on_event`).
struct LineWriter<'a> {
    buf: Vec<u8>,
    log: &'a ChannelWriter,
}

impl Write for LineWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LineWriter<'_> {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let line = escape_line_breaks(&self.buf);
        self.buf.clear();
        match self.log.tx.try_send(Msg::Line(line)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.log.dropped.fetch_add(1, Ordering::Relaxed);
            }
            // The guard stopped the writer (shutdown): nothing left to write to.
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

fn write_loop(rx: &Receiver<Msg>, mut sink: Box<dyn Write + Send>, dropped: &AtomicU64) {
    let mut reported = 0;
    while let Ok(mut msg) = rx.recv() {
        loop {
            match msg {
                Msg::Line(line) => {
                    let _ = sink.write_all(&line);
                }
                Msg::Stop(ack) => {
                    // Shutdown: the count of lost lines is written too, even when the stop arrived in the
                    // same batch as the last queued lines.
                    report_dropped(sink.as_mut(), dropped, &mut reported);
                    let _ = sink.flush();
                    let _ = ack.send(());
                    return;
                }
            }
            // Write what is queued, then flush once.
            msg = match rx.try_recv() {
                Ok(next) => next,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
        }
        report_dropped(sink.as_mut(), dropped, &mut reported);
        let _ = sink.flush();
    }
    // Every sender is gone without a stop (the guard gave up waiting): still report what was lost.
    report_dropped(sink.as_mut(), dropped, &mut reported);
    let _ = sink.flush();
}

/// Writes a note with the number of lines dropped since the last note, if any.
fn report_dropped(sink: &mut dyn Write, dropped: &AtomicU64, reported: &mut u64) {
    let total = dropped.load(Ordering::Relaxed);
    if total > *reported {
        let note = format!(
            "WARN smeltery::log: {} log lines dropped: the log file writer fell behind\n",
            total - *reported
        );
        *reported = total;
        let _ = sink.write_all(note.as_bytes());
    }
}

/// The log file, renamed to `<file>.1` when it would grow past `max` bytes.
struct RotatingFile {
    path: PathBuf,
    max: u64,
    file: Option<BufWriter<File>>,
    size: u64,
}

impl RotatingFile {
    fn open(path: PathBuf, max: u64) -> io::Result<Self> {
        let (file, size) = open_append(&path)?;
        Ok(Self {
            path,
            max,
            file: Some(file),
            size,
        })
    }

    fn backup(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(".1");
        PathBuf::from(name)
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }
        let backup = self.backup();
        // `rename` does not replace an existing file on every platform.
        let _ = std::fs::remove_file(&backup);
        std::fs::rename(&self.path, &backup)?;
        let (file, size) = open_append(&self.path)?;
        self.file = Some(file);
        self.size = size;
        Ok(())
    }
}

/// The log file's mode on Unix: the owner writes, its group (e.g. `adm`) may read, nobody else.
const FILE_MODE: u32 = 0o640;
/// The mode of log folders the framework creates.
const DIR_MODE: u32 = 0o750;

fn open_append(path: &Path) -> io::Result<(BufWriter<File>, u64)> {
    #[cfg(unix)]
    if crate::fsx::is_root() {
        return open_append_trusted(path, crate::fsx::ROOT_ONLY);
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        crate::fsx::create_dir_all(dir, DIR_MODE)?;
    }
    let file = crate::fsx::file_mode(OpenOptions::new().create(true).append(true), FILE_MODE)
        .open(path)?;
    let size = file.metadata()?.len();
    Ok((BufWriter::new(file), size))
}

/// Open the log as a process running as root: only in folders no other user controls
/// ([`crate::fsx::trusted_dir`]), never through a link at the file itself (`O_NOFOLLOW`), and only a file that
/// belongs to a `trusted` user. A setup command run with `sudo` in a folder the app user owns therefore logs to
/// stderr only (the caller reports why), instead of creating or appending to a file that user may have pointed
/// anywhere (`/etc/…`) with a symlink.
#[cfg(unix)]
fn open_append_trusted(path: &Path, trusted: &[u32]) -> io::Result<(BufWriter<File>, u64)> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "LOG_FILE names no file"))?;
    let dir = match path.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let dir = crate::fsx::trusted_dir(&dir, trusted, Some(DIR_MODE)).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("refusing to write the log file as root: {e}"),
        )
    })?;
    let nofollow = rustix::fs::OFlags::NOFOLLOW.bits();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .custom_flags(i32::try_from(nofollow).unwrap_or(i32::MAX))
        .open(dir.join(name))?;
    let meta = file.metadata()?;
    if !trusted.contains(&meta.uid()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to write the log file as root: {} belongs to user id {}",
                path.display(),
                meta.uid()
            ),
        ));
    }
    Ok((BufWriter::new(file), meta.len()))
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if self.size > 0 && self.size.saturating_add(len) > self.max {
            // A failed rotation keeps writing to the current file (or reopens it below).
            if self.rotate().is_err() && self.file.is_none() {
                let (file, size) = open_append(&self.path)?;
                self.file = Some(file);
                self.size = size;
            }
        }
        let file = match &mut self.file {
            Some(file) => file,
            None => {
                let (file, size) = open_append(&self.path)?;
                self.size = size;
                self.file.insert(file)
            }
        };
        if let Err(e) = file.write_all(buf) {
            // Start over with a fresh handle on the next line (the file may have been removed).
            self.file = None;
            return Err(e);
        }
        self.size = self.size.saturating_add(len);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

/// Tests: what `f` logs (every level), in the file format.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) fn capture(f: impl FnOnce()) -> String {
    struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let out = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (file, guard) = FileLog::with_sink(Box::new(Sink(Arc::clone(&out))), 4096).unwrap();
    tracing::subscriber::with_default(file_subscriber(file, "trace"), f);
    drop(guard);
    let bytes = out.lock().unwrap().clone();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_colours_only_on_a_terminal_without_no_color() {
        assert!(ansi_for(true, None));
        assert!(ansi_for(true, Some("".into())));
        assert!(!ansi_for(true, Some("1".into())));
        // Redirected stderr (a file, `docker logs`, journald) gets plain text.
        assert!(!ansi_for(false, None));
        assert!(!ansi_for(false, Some("1".into())));
    }

    #[test]
    fn events_become_plain_lines_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage/logs/smeltery.log");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "older line\n").unwrap();
        let (file, guard) = FileLog::open(&path, 10 * 1024 * 1024).unwrap();
        tracing::subscriber::with_default(file_subscriber(file, "info"), || {
            tracing::error!(user_id = 7, "boom");
            tracing::warn!("careful");
            tracing::debug!("hidden at info");
        });
        drop(guard);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(lines[0], "older line", "appended, not truncated");
        assert!(lines[1].starts_with("20"), "timestamp first: {text}");
        assert!(lines[1].contains("ERROR"), "{text}");
        assert!(lines[1].contains("boom") && lines[1].contains("user_id=7"));
        assert!(lines[2].contains("WARN") && lines[2].contains("careful"));
        assert!(!text.contains('\u{1b}'), "no colour codes: {text:?}");
    }

    #[test]
    fn the_parent_directory_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c.log");
        let (_file, guard) = FileLog::open(&path, 100).unwrap();
        drop(guard);
        assert!(path.is_file());
    }

    #[test]
    fn a_full_file_rotates_to_one_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        let (file, guard) = FileLog::open(&path, 300).unwrap();
        tracing::subscriber::with_default(file_subscriber(file, "info"), || {
            for i in 0..40 {
                tracing::info!(i, "line");
            }
        });
        drop(guard);
        let current = std::fs::read_to_string(&path).unwrap();
        let backup = std::fs::read_to_string(dir.path().join("app.log.1")).unwrap();
        assert!(current.len() <= 300, "{}", current.len());
        assert!(backup.len() <= 300, "{}", backup.len());
        assert!(current.contains("i=39"), "{current}");
        assert!(!dir.path().join("app.log.2").exists());
        // Every file ends on a whole line.
        assert!(current.ends_with('\n') && backup.ends_with('\n'));
    }

    /// A sink that blocks until the test lets it go: a disk that stopped answering.
    struct Stalled {
        gate: Receiver<()>,
        written: Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl Write for Stalled {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let _ = self.gate.recv();
            self.written
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_stalled_writer_drops_lines_instead_of_blocking() {
        let (gate_tx, gate) = mpsc::channel();
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Stalled {
            gate,
            written: Arc::clone(&written),
        };
        let (file, guard) = FileLog::with_sink(Box::new(sink), 4).unwrap();
        let started = Instant::now();
        tracing::subscriber::with_default(file_subscriber(file, "info"), || {
            for i in 0..200 {
                tracing::error!(i, "busy");
            }
        });
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "logging waited for the disk: {:?}",
            started.elapsed()
        );
        // At most the line in hand plus the channel's four slots got through.
        assert!(guard.dropped_lines() >= 195, "{}", guard.dropped_lines());
        // Let the disk answer again: every queued write and the drop note go through.
        for _ in 0..64 {
            let _ = gate_tx.send(());
        }
        drop(guard);
        let text = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        assert!(text.contains("i=0"), "{text}");
        assert!(text.contains("log lines dropped"), "{text}");
    }

    /// A sink the test reads back after the writer loop returned.
    struct Shared(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn run_write_loop(queued: Vec<Msg>, dropped: u64) -> String {
        let (tx, rx) = mpsc::sync_channel(queued.len().max(1));
        for msg in queued {
            tx.try_send(msg).unwrap();
        }
        drop(tx);
        let out = Arc::new(std::sync::Mutex::new(Vec::new()));
        write_loop(
            &rx,
            Box::new(Shared(Arc::clone(&out))),
            &AtomicU64::new(dropped),
        );
        String::from_utf8(out.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn the_drop_note_is_written_when_stop_comes_with_the_last_lines() {
        // The stop sits in the queue right behind the last lines, so the writer meets it while draining.
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let text = run_write_loop(
            vec![
                Msg::Line(b"a\n".to_vec()),
                Msg::Line(b"b\n".to_vec()),
                Msg::Stop(ack_tx),
            ],
            7,
        );
        assert!(ack_rx.try_recv().is_ok(), "the stop was acknowledged");
        assert_eq!(
            text,
            "a\nb\nWARN smeltery::log: 7 log lines dropped: the log file writer fell behind\n"
        );
    }

    #[test]
    fn the_drop_note_is_written_when_every_sender_is_gone() {
        let text = run_write_loop(vec![Msg::Line(b"a\n".to_vec())], 3);
        assert_eq!(
            text,
            "a\nWARN smeltery::log: 3 log lines dropped: the log file writer fell behind\n"
        );
        // Nothing lost, no note.
        assert_eq!(run_write_loop(vec![Msg::Line(b"a\n".to_vec())], 0), "a\n");
    }

    #[test]
    fn a_line_break_in_a_value_cannot_forge_a_log_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        let (file, guard) = FileLog::open(&path, 10 * 1024 * 1024).unwrap();
        // MySQL's duplicate-key error quotes the user's value.
        let detail = "Duplicate entry 'x\n2026-10-05T00:00:00Z  INFO smeltery: admin signed in\r' for key 'users.email'";
        tracing::subscriber::with_default(file_subscriber(file, "info"), || {
            tracing::error!(error = %detail, "internal error");
            tracing::warn!("{detail}");
        });
        drop(guard);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.contains("'x\\n2026-10-05T00:00:00Z"), "{text}");
        assert!(text.contains("admin signed in\\r'"), "{text}");
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn line_breaks_are_escaped_except_the_last() {
        assert_eq!(escape_line_breaks(b"a\nb\r\nc\n"), b"a\\nb\\r\\nc\n");
        assert_eq!(escape_line_breaks(b"plain\n"), b"plain\n");
        assert_eq!(escape_line_breaks(b"no end"), b"no end");
        assert_eq!(
            escape_line_breaks(b"\x1b[31mred\x1b[0m\n"),
            b"\x1b[31mred\x1b[0m\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_log_file_and_its_folder_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage/logs/smeltery.log");
        let (_file, guard) = FileLog::open(&path, 100).unwrap();
        drop(guard);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path) & 0o037, 0, "file {:o}", mode(&path));
        assert_eq!(
            mode(path.parent().unwrap()) & 0o027,
            0,
            "folder {:o}",
            mode(path.parent().unwrap())
        );
    }

    /// Root and this test's own user, which stands in for root in these tests.
    #[cfg(unix)]
    fn trusted() -> Vec<u32> {
        use std::os::unix::fs::MetadataExt as _;
        let probe = tempfile::NamedTempFile::new().unwrap();
        vec![0, probe.as_file().metadata().unwrap().uid()]
    }

    #[cfg(unix)]
    #[test]
    fn as_root_a_linked_log_file_is_never_followed() {
        let dir = crate::fsx::private_tempdir();
        let logs = dir.path().join("storage/logs");
        crate::fsx::private_mkdir(&dir.path().join("storage"));
        crate::fsx::private_mkdir(&logs);
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "root's file\n").unwrap();
        std::os::unix::fs::symlink(&victim, logs.join("smeltery.log")).unwrap();
        assert!(open_append_trusted(&logs.join("smeltery.log"), &trusted()).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "root's file\n");
        // A plain file in a private folder opens.
        assert!(open_append_trusted(&logs.join("other.log"), &trusted()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn as_root_a_folder_others_control_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::fsx::private_tempdir();
        let storage = dir.path().join("storage");
        std::fs::create_dir(&storage).unwrap();
        // Writable by others (the app user's folder, seen from root): `logs` could be a link to `/etc`.
        std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = open_append_trusted(&storage.join("logs/smeltery.log"), &trusted())
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("refusing to write the log file as root"),
            "{err}"
        );
        assert!(!storage.join("logs").exists());
        // A folder of another user: trust only an id nobody has.
        let err = open_append_trusted(&dir.path().join("x.log"), &[u32::MAX - 7])
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("belongs to user id"), "{err}");
    }

    #[test]
    fn levels_parse_with_info_as_the_fallback() {
        assert_eq!(level_filter("DEBUG"), LevelFilter::DEBUG);
        assert_eq!(level_filter("warn"), LevelFilter::WARN);
        assert_eq!(level_filter("nonsense"), LevelFilter::INFO);
    }
}
