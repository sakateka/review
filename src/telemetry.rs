//! Local-only startup and navigation timings appended as JSON lines.
//!
//! Records hold durations, counts, byte sizes, language names and enums only:
//! never paths, file names, symbols, queries, Git arguments, refs or
//! environment values. Every failure is ignored; nothing here may change the
//! review outcome. Nothing is recorded until `init` installs the sink.
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    io::Write,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Rotate the log once it reaches this size; at most two files are kept.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
const FORMAT_VERSION: u64 = 1;

static START: OnceLock<Instant> = OnceLock::new();
static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static CAPTURED: std::cell::RefCell<Vec<Value>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// An append-only JSON-lines file with size-capped rotation.
pub struct Sink {
    path: PathBuf,
    session: String,
}

impl Sink {
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            session: session_id(),
        }
    }

    /// `$XDG_STATE_HOME/review/telemetry.jsonl`, else
    /// `$HOME/.local/state/review/telemetry.jsonl`; a relative base is refused.
    pub fn default_path() -> Option<PathBuf> {
        Self::path_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
    }

    pub fn path_from(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
        let base = match xdg_state_home.filter(|path| !path.is_empty()) {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(home.filter(|path| !path.is_empty())?).join(".local/state"),
        };
        base.is_absolute()
            .then(|| base.join("review/telemetry.jsonl"))
    }

    /// Append one event; the caller ignores the error.
    pub fn append(&self, event: &str, fields: Value) -> std::io::Result<()> {
        let record = record_value(&self.session, event, fields);
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::metadata(&self.path).is_ok_and(|meta| meta.len() >= MAX_FILE_BYTES) {
            let mut rotated = self.path.clone().into_os_string();
            rotated.push(".1");
            std::fs::rename(&self.path, rotated)?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?
            .write_all(&line)
    }
}

/// The common envelope around event-specific fields.
pub fn record_value(session: &str, event: &str, fields: Value) -> Value {
    let mut record = Map::new();
    record.insert("v".into(), json!(FORMAT_VERSION));
    record.insert("event".into(), json!(event));
    record.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    record.insert("session".into(), json!(session));
    record.insert(
        "ts".into(),
        json!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_millis() as u64)
        ),
    );
    if let Value::Object(fields) = fields {
        record.extend(fields);
    }
    Value::Object(record)
}

fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos() as u64);
    let mixed = (nanos ^ (u64::from(std::process::id()) << 32)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    format!("{mixed:016x}")
}

/// Mark the process start; the first call wins.
pub fn start() {
    START.get_or_init(Instant::now);
}

/// Time since `start` (zero when it was never called).
pub fn since_start() -> Duration {
    START.get().map_or(Duration::ZERO, Instant::elapsed)
}

/// Install the global sink; only `main::run` calls this.
pub fn init(enabled: bool) {
    if enabled && let Some(path) = Sink::default_path() {
        let _ = SINK.set(Mutex::new(Sink::at(path)));
    }
}

pub fn enabled() -> bool {
    SINK.get().is_some()
}

/// Record one event; silently does nothing without a sink or on any error.
pub fn record(event: &str, fields: Value) {
    #[cfg(test)]
    CAPTURED.with(|captured| {
        captured
            .borrow_mut()
            .push(record_value("test", event, fields.clone()))
    });
    if let Some(sink) = SINK.get()
        && let Ok(sink) = sink.lock()
    {
        let _ = sink.append(event, fields);
    }
}

/// Events recorded on this test thread since the last call.
#[cfg(test)]
pub fn take_captured() -> Vec<Value> {
    CAPTURED.with(|captured| std::mem::take(&mut *captured.borrow_mut()))
}

/// Milliseconds with microsecond precision.
pub fn ms(duration: Duration) -> f64 {
    duration.as_micros() as f64 / 1000.0
}

/// A language name for a `symbols::classify` selector.
pub fn language(selector: &str) -> &'static str {
    match selector {
        "buffer.rs" => "rust",
        "buffer.c" => "c",
        "buffer.cpp" => "cpp",
        "buffer.go" => "go",
        "buffer.zig" => "zig",
        "buffer.py" => "python",
        "buffer.sh" => "shell",
        "yaml" => "yaml",
        "json" => "json",
        _ => "other",
    }
}

/// The language of one captured source, if it is supported.
pub fn source_language(source: &crate::source::CapturedSource) -> Option<&'static str> {
    let text = std::str::from_utf8(&source.bytes).ok()?;
    crate::symbols::classify(&source.path, text).map(language)
}

/// Startup phases measured before the terminal is set up.
#[derive(Debug, Clone, Default)]
pub struct Startup {
    pub config: Duration,
    pub capture: Duration,
    pub app: Duration,
    pub keymap: &'static str,
}

/// The `startup` event fields.
pub fn startup_fields(
    startup: &Startup,
    terminal: Duration,
    first_frame: Duration,
    capture: &crate::git::Capture,
) -> Value {
    use crate::git::Statistics;
    let (mut added, mut removed) = (0usize, 0usize);
    for item in &capture.items {
        if let Statistics::Text {
            additions,
            deletions,
        } = item.statistics()
        {
            added = added.saturating_add(additions);
            removed = removed.saturating_add(deletions);
        }
    }
    json!({
        "config_ms": ms(startup.config),
        "capture_ms": ms(startup.capture),
        "git_diff_ms": ms(capture.git_diff),
        "app_ms": ms(startup.app),
        "terminal_ms": ms(terminal),
        "first_frame_ms": ms(first_frame),
        "files": capture.items.len(),
        "patch_bytes": capture.original.len(),
        "added_lines": added,
        "removed_lines": removed,
        "sources": capture.catalog.sources.len(),
        "source_bytes": source_bytes(&capture.catalog),
        "structured": capture.unavailable.is_none(),
        "keymap": startup.keymap,
    })
}

fn source_bytes(catalog: &crate::source::Catalog) -> usize {
    catalog
        .sources
        .iter()
        .map(|source| source.bytes.len())
        .sum()
}

/// The `analysis` event fields.
pub fn analysis_fields(
    catalog: &crate::source::Catalog,
    build: Duration,
    tags: usize,
    panicked: bool,
) -> Value {
    let mut languages = std::collections::BTreeMap::<&str, usize>::new();
    for language in catalog.sources.iter().filter_map(source_language) {
        *languages.entry(language).or_default() += 1;
    }
    json!({
        "build_ms": ms(build),
        "sources": catalog.sources.len(),
        "source_bytes": source_bytes(catalog),
        "tags": tags,
        "languages": languages,
        "panicked": panicked,
    })
}

/// A Definitions/References request awaiting its results.
#[derive(Debug, Clone)]
pub struct PendingNavigation {
    pub generation: u64,
    pub operation: &'static str,
    pub submitted: Instant,
    pub waited_for_index: bool,
    pub language: Option<&'static str>,
}

/// The `navigation` event fields once results are applied.
pub fn navigation_fields(pending: &PendingNavigation, query: Duration, candidates: usize) -> Value {
    json!({
        "operation": pending.operation,
        "latency_ms": ms(pending.submitted.elapsed()),
        "waited_for_index": pending.waited_for_index,
        "query_ms": ms(query),
        "candidates": candidates,
        "language": pending.language,
    })
}
