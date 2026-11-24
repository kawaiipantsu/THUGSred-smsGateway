//! Structured logging: four log files routed by target, optional journal
//! output, size-independent time based rotation with retention.
//!
//! Targets `messages`, `integrations` and `audit` go to their own files;
//! everything else goes to the system log.

use crate::config::Config;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use tracing::level_filters::LevelFilter;
use tracing::Metadata;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::{Layered, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{filter, reload, Layer, Registry};

/// File that is renamed to `<name>.<period>` when the period changes.
pub struct RotatingFile {
    path: PathBuf,
    /// daily, hourly or never
    rotation: String,
    retention_days: u32,
    file: Option<File>,
    period: String,
}

impl RotatingFile {
    pub fn open(path: &str, rotation: &str, retention_days: u32) -> io::Result<Self> {
        let path = PathBuf::from(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut me = Self {
            path,
            rotation: rotation.to_string(),
            retention_days,
            file: None,
            period: String::new(),
        };
        // An existing file belongs to the period in which it was last written.
        me.period = std::fs::metadata(&me.path)
            .and_then(|m| m.modified())
            .map(|t| me.period_of(chrono::DateTime::<chrono::Utc>::from(t)))
            .unwrap_or_else(|_| me.period_of(chrono::Utc::now()));
        me.rotate_if_needed()?;
        me.file = Some(me.open_file()?);
        Ok(me)
    }

    fn period_of(&self, t: chrono::DateTime<chrono::Utc>) -> String {
        match self.rotation.as_str() {
            "hourly" => t.format("%Y-%m-%d-%H").to_string(),
            "never" => String::new(),
            _ => t.format("%Y-%m-%d").to_string(),
        }
    }

    fn open_file(&self) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o640)
            .open(&self.path)
    }

    fn rotate_if_needed(&mut self) -> io::Result<()> {
        let now = self.period_of(chrono::Utc::now());
        if now == self.period {
            return Ok(());
        }
        let old = std::mem::replace(&mut self.period, now);
        self.file = None;
        if std::fs::metadata(&self.path)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
        {
            let mut target = self.path.clone().into_os_string();
            target.push(format!(".{old}"));
            std::fs::rename(&self.path, target)?;
        }
        self.prune();
        Ok(())
    }

    /// Remove rotated files older than the retention period.
    fn prune(&self) {
        if self.retention_days == 0 {
            return;
        }
        let (Some(dir), Some(name)) = (
            self.path.parent(),
            self.path.file_name().and_then(|n| n.to_str()),
        ) else {
            return;
        };
        let cutoff = std::time::SystemTime::now()
            - std::time::Duration::from_secs(self.retention_days as u64 * 86400);
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let fname = e.file_name();
            let Some(f) = fname.to_str() else { continue };
            if f.len() > name.len() + 1
                && f.starts_with(name)
                && f.as_bytes()[name.len()] == b'.'
                && e.metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t < cutoff)
                    .unwrap_or(false)
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.rotate_if_needed()?;
        if self.file.is_none() {
            self.file = Some(self.open_file()?);
        }
        match self.file.as_mut() {
            Some(f) => f.write(buf),
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

type Base = Layered<reload::Layer<LevelFilter, Registry>, Registry>;
type BoxedLayer = Box<dyn Layer<Base> + Send + Sync>;

pub struct LogHandle {
    _guards: Vec<WorkerGuard>,
    level: reload::Handle<LevelFilter, Registry>,
}

pub fn parse_level(s: &str) -> LevelFilter {
    match s {
        "trace" => LevelFilter::TRACE,
        "debug" => LevelFilter::DEBUG,
        "warn" => LevelFilter::WARN,
        "error" => LevelFilter::ERROR,
        _ => LevelFilter::INFO,
    }
}

impl LogHandle {
    /// Change the verbosity at runtime (configuration reload).
    pub fn set_level(&self, level: &str) {
        let _ = self.level.modify(|f| *f = parse_level(level));
    }
}

const DEDICATED: [&str; 3] = ["messages", "integrations", "audit"];

/// Third-party crates are limited to warnings so that verbose levels do not
/// flood the logs (or record SQL statements).
fn own_or_important(meta: &Metadata<'_>) -> bool {
    let t = meta.target();
    t.starts_with("thugsred") || DEDICATED.contains(&t) || *meta.level() <= tracing::Level::WARN
}

fn file_layer(
    path: &str,
    cfg: &Config,
    guards: &mut Vec<WorkerGuard>,
    accept: impl Fn(&str) -> bool + Send + Sync + 'static,
) -> Option<BoxedLayer> {
    let l = &cfg.logging;
    let file = match RotatingFile::open(path, &l.rotation, l.retention_days) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("warning: cannot open log file {path}: {e}; this log is disabled");
            return None;
        }
    };
    let (writer, guard) = tracing_appender::non_blocking(file);
    guards.push(guard);
    let flt = filter::filter_fn(move |m| accept(m.target()) && own_or_important(m));
    Some(if l.format == "json" {
        tracing_subscriber::fmt::layer()
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .with_span_list(false)
            .with_writer(writer)
            .with_filter(flt)
            .boxed()
    } else {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
            .with_filter(flt)
            .boxed()
    })
}

/// Initialise logging. With `files` false only the console/journal is used
/// (command-line tools).
pub fn init(cfg: &Config, files: bool) -> LogHandle {
    let (level_layer, level) = reload::Layer::new(parse_level(&cfg.general.log_level));
    let mut guards = Vec::new();
    let mut layers: Vec<BoxedLayer> = Vec::new();
    let l = &cfg.logging;
    if files {
        layers.extend(file_layer(&l.system_log, cfg, &mut guards, |t| {
            !DEDICATED.contains(&t)
        }));
        layers.extend(file_layer(&l.messages_log, cfg, &mut guards, |t| {
            t == "messages"
        }));
        layers.extend(file_layer(&l.integrations_log, cfg, &mut guards, |t| {
            t == "integrations"
        }));
        layers.extend(file_layer(&l.audit_log, cfg, &mut guards, |t| t == "audit"));
    }
    if l.journal_enabled || !files {
        // systemd captures stderr into the journal and adds its own timestamps.
        let under_systemd = std::env::var_os("JOURNAL_STREAM").is_some();
        let flt = filter::filter_fn(own_or_important);
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_ansi(false)
            .with_target(true);
        layers.push(if under_systemd {
            layer.without_time().with_filter(flt).boxed()
        } else {
            layer.with_filter(flt).boxed()
        });
    }
    let _ = tracing_subscriber::registry()
        .with(level_layer)
        .with(layers)
        .try_init();
    LogHandle {
        _guards: guards,
        level,
    }
}

/// Directory of a log file, for diagnostics.
pub fn log_dir(path: &str) -> &Path {
    Path::new(path).parent().unwrap_or(Path::new("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_prunes() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("system.log");
        let mut f = RotatingFile::open(p.to_str().unwrap(), "daily", 1).unwrap();
        f.write_all(b"one\n").unwrap();
        f.flush().unwrap();
        // Simulate the period having changed.
        f.period = "2000-01-01".into();
        f.write_all(b"two\n").unwrap();
        f.flush().unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join("system.log.2000-01-01")).unwrap(),
            "one\n"
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two\n");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o640
        );
        // An old rotated file is removed by pruning; unrelated files are kept.
        let old = d.path().join("system.log.1999-01-01");
        std::fs::write(&old, "x").unwrap();
        std::fs::write(d.path().join("system.logger"), "keep").unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86400);
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(past)
            .unwrap();
        f.prune();
        assert!(!old.exists());
        assert!(d.path().join("system.logger").exists());
        assert!(d.path().join("system.log.2000-01-01").exists());
    }

    #[test]
    fn never_rotation_keeps_one_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.log");
        let mut f = RotatingFile::open(p.to_str().unwrap(), "never", 30).unwrap();
        f.write_all(b"x").unwrap();
        f.write_all(b"y").unwrap();
        f.flush().unwrap();
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
