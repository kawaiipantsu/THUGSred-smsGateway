//! Shared harness: a complete gateway running against simulated modems.
#![allow(dead_code)]

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use thugsred_smsgateway::config::{self, LoadOptions};
use thugsred_smsgateway::daemon::Gateway;
use thugsred_smsgateway::modem::simulator::{SimConnector, SimHandle};
use thugsred_smsgateway::modem::transport::Connector;

pub struct Harness {
    pub gw: Arc<Gateway>,
    pub sims: Vec<SimHandle>,
    pub dir: tempfile::TempDir,
    pub options: LoadOptions,
}

pub struct Setup {
    pub main: String,
    pub sim: String,
    pub integrations: String,
    pub providers: String,
    pub modems: usize,
    pub sims: Vec<SimHandle>,
    pub dir: Option<tempfile::TempDir>,
    /// `section.field=value` overrides for sections the harness writes itself.
    pub overrides: Vec<String>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            main: String::new(),
            sim: String::new(),
            integrations: String::new(),
            providers: String::new(),
            modems: 1,
            sims: Vec::new(),
            dir: None,
            overrides: Vec::new(),
        }
    }
}

/// Create a uniquely named MySQL database when `TEST_MYSQL_URL` is set.
async fn mysql_url() -> Option<String> {
    let base = std::env::var("TEST_MYSQL_URL").ok()?;
    sqlx::any::install_default_drivers();
    let name = format!("t_{}", uuid::Uuid::new_v4().simple());
    let admin = sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .connect(&base)
        .await
        .unwrap();
    sqlx::query(&format!("CREATE DATABASE {name} CHARACTER SET utf8mb4"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    Some(format!("{}/{name}", base.trim_end_matches('/')))
}

pub fn write_config(dir: &std::path::Path, s: &Setup, mysql: Option<&str>) {
    let p = |n: &str| dir.join(n).display().to_string();
    let db = match mysql {
        Some(url) => format!("driver = \"mysql\"\nmysql_url = \"{url}\""),
        None => format!("sqlite_path = \"{}\"", p("messages.db")),
    };
    let mut main = format!(
        "[general]\ninstance_id = \"test-gw\"\nshutdown_timeout_seconds = 5\n[paths]\nhome = \"{}\"\ndata = \"{}\"\nruntime = \"{}\"\nrecordings = \"{}\"\nattachments = \"{}\"\nbackups = \"{}\"\nmodem_profiles = \"{}\"\nsim_config = \"{}\"\naddressbook = \"{}\"\nanonymous = \"{}\"\nproviders = \"{}\"\n[integrations]\nconfig_file = \"{}\"\nretry_initial_seconds = 1\nretry_max_seconds = 2\n[database]\n{db}\n[modem]\npoll_interval_seconds = 1\nhealthcheck_interval_seconds = 1\nreconnect_delay_seconds = 1\nmax_reconnect_delay_seconds = 1\ncommand_timeout_seconds = 2\nmessage_timeout_seconds = 2\n",
        p("home"), p("data"), p("run"), p("recordings"), p("attachments"), p("backups"), p("profiles"), p("sim.conf"), p("addressbook.conf"), p("anonymous.conf"), p("providers.conf"), p("integrations.conf"),
    );
    for i in 0..s.modems {
        if s.modems > 1 {
            main.push_str(&format!(
                "[[modems]]\nid = \"modem-{0}\"\ndevice = \"/dev/sim{0}\"\nsim = \"sim-{0}\"\n",
                i + 1
            ));
        }
    }
    main.push_str(&s.main);
    std::fs::write(dir.join("smsgateway.conf"), main).unwrap();
    let sim = if s.sim.is_empty() && s.modems > 1 {
        (0..s.modems)
            .map(|i| {
                format!(
                    "[[sims]]\nid = \"sim-{0}\"\nmobile_number = \"+450000010{0}\"\n",
                    i + 1
                )
            })
            .collect()
    } else {
        s.sim.clone()
    };
    std::fs::write(dir.join("sim.conf"), sim).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.join("sim.conf"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(dir.join("integrations.conf"), &s.integrations).unwrap();
    if !s.providers.is_empty() {
        std::fs::write(dir.join("providers.conf"), &s.providers).unwrap();
        std::fs::set_permissions(
            dir.join("providers.conf"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
}

pub async fn start(mut s: Setup) -> Harness {
    let dir = s.dir.take().unwrap_or_else(|| tempfile::tempdir().unwrap());
    let fresh = !dir.path().join("smsgateway.conf").exists();
    if fresh {
        let url = mysql_url().await;
        write_config(dir.path(), &s, url.as_deref());
    }
    let mut options = LoadOptions::new(dir.path().join("smsgateway.conf"));
    options.use_env = false;
    options.overrides = s.overrides.clone();
    let cfg = config::load(&options).unwrap_or_else(|e| panic!("{e}"));
    while s.sims.len() < cfg.modems.len() {
        s.sims.push(SimHandle::new());
    }
    let sims = s.sims.clone();
    let ids: Vec<String> = cfg.modems.iter().map(|m| m.id.clone()).collect();
    let gw = Gateway::start(cfg, options.clone(), move |id| {
        let idx = ids.iter().position(|x| x == id).unwrap_or(0);
        Arc::new(SimConnector(sims[idx].clone())) as Arc<dyn Connector>
    })
    .await
    .unwrap();
    Harness {
        gw,
        sims: s.sims,
        dir,
        options,
    }
}

impl Harness {
    pub fn sim(&self) -> &SimHandle {
        &self.sims[0]
    }

    pub async fn ready(&self) {
        for m in &self.gw.modems {
            let m = m.clone();
            wait("modem ready", 15, || {
                let m = m.clone();
                async move { m.is_ready() }
            })
            .await;
        }
    }

    pub async fn message_status(&self, id: &str) -> String {
        self.gw
            .db
            .get_message(id)
            .await
            .unwrap()
            .map(|m| m.status)
            .unwrap_or_default()
    }

    pub async fn wait_status(&self, id: &str, status: &str) {
        let (db, id2, want) = (self.gw.db.clone(), id.to_string(), status.to_string());
        wait(&format!("message status {status}"), 20, || {
            let (db, id, want) = (db.clone(), id2.clone(), want.clone());
            async move {
                db.get_message(&id)
                    .await
                    .unwrap()
                    .map(|m| m.status == want)
                    .unwrap_or(false)
            }
        })
        .await;
    }

    pub async fn inbound(&self) -> Vec<thugsred_smsgateway::database::Message> {
        let q = thugsred_smsgateway::database::MessageQuery {
            direction: Some("inbound".into()),
            limit: 100,
            ascending: true,
            ..Default::default()
        };
        self.gw.db.search_messages(&q).await.unwrap().0
    }

    pub async fn wait_inbound(&self, n: usize) -> Vec<thugsred_smsgateway::database::Message> {
        for _ in 0..200 {
            let m = self.inbound().await;
            if m.len() >= n {
                return m;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "timed out waiting for {n} inbound message(s); have {}",
            self.inbound().await.len()
        );
    }

    pub async fn events(&self, kind: &str) -> Vec<thugsred_smsgateway::database::EventRecord> {
        self.gw
            .db
            .list_events(Some(kind), None, 500, 0)
            .await
            .unwrap()
            .0
    }

    /// Stop the gateway and return the directory for a restart.
    pub async fn stop(self) -> (tempfile::TempDir, Vec<SimHandle>) {
        self.gw.shutdown().await;
        (self.dir, self.sims)
    }
}

pub async fn wait<F, Fut>(what: &str, seconds: u64, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while tokio::time::Instant::now() < deadline {
        if cond().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    panic!("timed out waiting for: {what}");
}

pub fn ts() -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::parse_from_rfc3339("2025-03-04T05:06:07+01:00").unwrap()
}

/// Serve the gateway's HTTP API on an ephemeral local port.
pub async fn serve(h: &Harness) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(thugsred_smsgateway::api::serve_on(h.gw.clone(), listener));
    format!("http://{addr}")
}

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: std::collections::HashMap<String, String>,
    pub body: String,
}

#[derive(Clone, Default)]
pub struct Mock {
    pub requests: Arc<std::sync::Mutex<Vec<Recorded>>>,
    /// HTTP status returned to callers (default 200).
    pub status: Arc<std::sync::atomic::AtomicU16>,
}

impl Mock {
    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    pub fn set_status(&self, s: u16) {
        self.status.store(s, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Local HTTP endpoint that records every request (stand-in for webhooks).
pub async fn mock_server() -> (String, Mock) {
    use axum::extract::{Request, State};
    let mock = Mock::default();
    mock.set_status(200);
    async fn handler(State(m): State<Mock>, req: Request) -> axum::http::StatusCode {
        let (parts, body) = req.into_parts();
        let bytes = axum::body::to_bytes(body, 1 << 20)
            .await
            .unwrap_or_default();
        m.requests.lock().unwrap().push(Recorded {
            method: parts.method.to_string(),
            path: parts.uri.path().to_string(),
            query: parts.uri.query().unwrap_or("").to_string(),
            headers: parts
                .headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect(),
            body: String::from_utf8_lossy(&bytes).to_string(),
        });
        axum::http::StatusCode::from_u16(m.status.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap()
    }
    let app = axum::Router::new()
        .fallback(handler)
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), mock)
}
