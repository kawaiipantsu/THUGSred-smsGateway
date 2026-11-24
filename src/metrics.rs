//! Minimal Prometheus-compatible metrics registry.
//!
//! Labels are limited to low-cardinality values (modem id, integration name,
//! route template, status). Telephone numbers and message content are never
//! used as labels.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

const DESCRIPTIONS: &[(&str, &str, &str)] = &[
    (
        "smsgw_messages_received_total",
        "counter",
        "Inbound messages persisted",
    ),
    (
        "smsgw_messages_submitted_total",
        "counter",
        "Outbound messages accepted by the network",
    ),
    (
        "smsgw_message_parts_submitted_total",
        "counter",
        "Outbound SMS segments accepted by the network",
    ),
    (
        "smsgw_messages_delivered_total",
        "counter",
        "Outbound messages confirmed delivered",
    ),
    (
        "smsgw_message_failures_total",
        "counter",
        "Outbound messages that failed",
    ),
    (
        "smsgw_queue_depth",
        "gauge",
        "Messages waiting in the outbound queue",
    ),
    (
        "smsgw_integration_attempts_total",
        "counter",
        "Integration delivery attempts",
    ),
    (
        "smsgw_integration_failures_total",
        "counter",
        "Failed integration delivery attempts",
    ),
    (
        "smsgw_modem_connected",
        "gauge",
        "1 when the modem is connected and ready",
    ),
    (
        "smsgw_modem_signal_dbm",
        "gauge",
        "Received signal strength in dBm",
    ),
    (
        "smsgw_modem_reconnect_attempts_total",
        "counter",
        "Modem reconnect attempts",
    ),
    (
        "smsgw_database_up",
        "gauge",
        "1 when the database answers queries",
    ),
    (
        "smsgw_process_uptime_seconds",
        "gauge",
        "Seconds since the daemon started",
    ),
    (
        "smsgw_api_requests_total",
        "counter",
        "API requests by route and status",
    ),
    (
        "smsgw_api_request_duration_seconds",
        "histogram",
        "API request latency",
    ),
    (
        "smsgw_disk_free_bytes",
        "gauge",
        "Free bytes on the data filesystem",
    ),
    (
        "smsgw_disk_total_bytes",
        "gauge",
        "Total bytes on the data filesystem",
    ),
    ("smsgw_events_total", "counter", "Gateway events emitted"),
    ("smsgw_calls_total", "counter", "Calls observed"),
];

const BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Default)]
struct Hist {
    buckets: [u64; 11],
    sum: f64,
    count: u64,
}

#[derive(Default)]
struct Inner {
    counters: BTreeMap<(String, String), f64>,
    gauges: BTreeMap<(String, String), f64>,
    hists: BTreeMap<(String, String), Hist>,
}

pub struct Metrics {
    started: Instant,
    inner: Mutex<Inner>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

fn label_str(labels: &[(&str, &str)]) -> String {
    labels
        .iter()
        .map(|(k, v)| {
            let v = v
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', " ");
            format!("{k}=\"{v}\"")
        })
        .collect::<Vec<_>>()
        .join(",")
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    pub fn inc(&self, name: &str, labels: &[(&str, &str)]) {
        self.add(name, labels, 1.0);
    }

    pub fn add(&self, name: &str, labels: &[(&str, &str)], v: f64) {
        *self
            .lock()
            .counters
            .entry((name.to_string(), label_str(labels)))
            .or_insert(0.0) += v;
    }

    pub fn set(&self, name: &str, labels: &[(&str, &str)], v: f64) {
        self.lock()
            .gauges
            .insert((name.to_string(), label_str(labels)), v);
    }

    pub fn observe(&self, name: &str, labels: &[(&str, &str)], seconds: f64) {
        let mut g = self.lock();
        let h = g
            .hists
            .entry((name.to_string(), label_str(labels)))
            .or_default();
        for (i, b) in BUCKETS.iter().enumerate() {
            if seconds <= *b {
                h.buckets[i] += 1;
            }
        }
        h.sum += seconds;
        h.count += 1;
    }

    /// Render the Prometheus text exposition format.
    pub fn render(&self) -> String {
        self.set(
            "smsgw_process_uptime_seconds",
            &[],
            self.started.elapsed().as_secs_f64(),
        );
        let g = self.lock();
        let mut out = String::new();
        let series = |name: &str, labels: &str, v: f64| {
            if labels.is_empty() {
                format!("{name} {v}\n")
            } else {
                format!("{name}{{{labels}}} {v}\n")
            }
        };
        for (name, kind, help) in DESCRIPTIONS {
            let mut body = String::new();
            match *kind {
                "counter" => {
                    for ((n, l), v) in g.counters.iter().filter(|((n, _), _)| n == name) {
                        body.push_str(&series(n, l, *v));
                    }
                }
                "gauge" => {
                    for ((n, l), v) in g.gauges.iter().filter(|((n, _), _)| n == name) {
                        body.push_str(&series(n, l, *v));
                    }
                }
                _ => {
                    for ((n, l), h) in g.hists.iter().filter(|((n, _), _)| n == name) {
                        let sep = if l.is_empty() { "" } else { "," };
                        for (i, b) in BUCKETS.iter().enumerate() {
                            body.push_str(&format!(
                                "{n}_bucket{{{l}{sep}le=\"{b}\"}} {}\n",
                                h.buckets[i]
                            ));
                        }
                        body.push_str(&format!("{n}_bucket{{{l}{sep}le=\"+Inf\"}} {}\n", h.count));
                        body.push_str(&series(&format!("{n}_sum"), l, h.sum));
                        body.push_str(&series(&format!("{n}_count"), l, h.count as f64));
                    }
                }
            }
            if !body.is_empty() {
                out.push_str(&format!(
                    "# HELP {name} {help}\n# TYPE {name} {kind}\n{body}"
                ));
            }
        }
        out
    }
}

/// Free and total bytes of the filesystem holding `path`.
pub fn disk_usage(path: &str) -> Option<(u64, u64)> {
    let st = nix::sys::statvfs::statvfs(path).ok()?;
    let frsize = st.fragment_size() as u64;
    Some((
        st.blocks_available() as u64 * frsize,
        st.blocks() as u64 * frsize,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_prometheus_text() {
        let m = Metrics::new();
        m.inc("smsgw_messages_received_total", &[("modem", "modem-1")]);
        m.inc("smsgw_messages_received_total", &[("modem", "modem-1")]);
        m.set("smsgw_queue_depth", &[], 3.0);
        m.observe(
            "smsgw_api_request_duration_seconds",
            &[("route", "/health")],
            0.02,
        );
        let out = m.render();
        assert!(out.contains("# TYPE smsgw_messages_received_total counter"));
        assert!(out.contains("smsgw_messages_received_total{modem=\"modem-1\"} 2"));
        assert!(out.contains("smsgw_queue_depth 3"));
        assert!(out.contains(
            "smsgw_api_request_duration_seconds_bucket{route=\"/health\",le=\"0.025\"} 1"
        ));
        assert!(out.contains("smsgw_api_request_duration_seconds_count{route=\"/health\"} 1"));
        assert!(out.contains("smsgw_process_uptime_seconds"));
    }

    #[test]
    fn disk_usage_works() {
        let (free, total) = disk_usage("/").unwrap();
        assert!(total >= free && total > 0);
    }
}
