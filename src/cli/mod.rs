//! Command-line interface. Commands that need the modem talk to the running
//! daemon over its HTTP API; they never open the serial device themselves.

mod doctor;
mod run;

use crate::config::{self, Config, LoadOptions};
use crate::database::Db;
use crate::sms::pdu::{self, Encoding, Payload, SubmitRequest};
use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "thugsred-smsgateway", version = crate::daemon::VERSION, about = "THUGSred SMS Gateway - self-hosted GSM/SMS gateway (https://thugs.red)")]
pub struct Cli {
    /// Path of the main configuration file.
    #[arg(
        short,
        long,
        global = true,
        env = "THUGSRED_SMSGW_CONFIG",
        default_value = "/etc/thugsred/smsgateway.conf"
    )]
    pub config: PathBuf,
    /// Override a setting: section.field=value (repeatable).
    #[arg(long = "set", global = true, value_name = "KEY=VALUE")]
    pub overrides: Vec<String>,
    /// Print machine-readable JSON where supported.
    #[arg(long, global = true)]
    pub json: bool,
    /// Base URL of the running daemon (default: derived from the configuration).
    #[arg(long, global = true, value_name = "URL")]
    pub api_url: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the gateway daemon.
    Run {
        /// Use the built-in modem simulator instead of serial hardware.
        #[arg(long)]
        simulate_modem: bool,
    },
    /// Print version and build information.
    Version,
    /// Validate the configuration files.
    CheckConfig {
        /// Print the effective configuration with secrets redacted.
        #[arg(long)]
        show: bool,
    },
    /// Show the status of the running daemon.
    Status,
    /// Diagnose the installation and environment.
    Doctor,
    /// Show gateway and modem capabilities.
    Capabilities,
    /// Send, list and search messages.
    Sms {
        #[command(subcommand)]
        command: SmsCommand,
    },
    /// Offline PDU encoder and decoder.
    Pdu {
        #[command(subcommand)]
        command: PduCommand,
    },
    /// Modem information and control.
    Modem {
        #[command(subcommand)]
        command: ModemCommand,
        /// Modem id (default: first modem).
        #[arg(long, global = true)]
        modem: Option<String>,
    },
    /// Database maintenance.
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
    /// Integration management.
    Integrations {
        #[command(subcommand)]
        command: IntegrationCommand,
    },
    /// Run a configured USSD command through the daemon.
    Ussd {
        /// Name from [ussd.commands].
        command: String,
        #[arg(long)]
        modem: Option<String>,
    },
    /// Ask the running daemon to reload its configuration.
    Reload,
    /// Print a diagnostic report with all secrets redacted.
    Diagnostics,
    /// Run a software modem on a pseudo-terminal (development).
    Simulator {
        /// Create a symbolic link to the pseudo-terminal at this path.
        #[arg(long)]
        link: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum SmsCommand {
    /// Queue an SMS through the running daemon.
    Send(SendArgs),
    /// List recent messages.
    List {
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// inbound or outbound
        #[arg(long)]
        direction: Option<String>,
    },
    /// Search message history.
    Search {
        #[arg(long)]
        sender: Option<String>,
        #[arg(long)]
        recipient: Option<String>,
        /// Text contained in the body, sender or recipient.
        #[arg(long)]
        text: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
}

#[derive(Args)]
pub struct SendArgs {
    /// Destination telephone number.
    #[arg(long)]
    pub to: String,
    #[arg(long)]
    pub message: String,
    /// Send as a flash (class 0) message.
    #[arg(long)]
    pub flash: bool,
    /// Send through this modem.
    #[arg(long)]
    pub modem: Option<String>,
    /// Send through the SIM with this gateway number.
    #[arg(long)]
    pub from: Option<String>,
    /// Schedule for an RFC 3339 time.
    #[arg(long, value_name = "TIME")]
    pub at: Option<String>,
    /// Do not request a delivery report.
    #[arg(long)]
    pub no_delivery_report: bool,
    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Subcommand)]
pub enum PduCommand {
    /// Encode a message into SMS-SUBMIT PDUs (nothing is sent).
    Encode {
        #[arg(long)]
        to: String,
        #[arg(long)]
        message: Option<String>,
        /// Binary payload as hexadecimal.
        #[arg(long)]
        binary: Option<String>,
        /// auto, gsm7, ucs2 or binary
        #[arg(long, default_value = "auto")]
        encoding: String,
        #[arg(long)]
        smsc: Option<String>,
        #[arg(long)]
        flash: bool,
        #[arg(long)]
        delivery_report: bool,
        #[arg(long)]
        validity_minutes: Option<u32>,
        #[arg(long)]
        destination_port: Option<u16>,
        #[arg(long)]
        source_port: Option<u16>,
    },
    /// Decode a hexadecimal PDU (read from standard input when omitted).
    Decode {
        pdu: Option<String>,
        /// auto, deliver or submit
        #[arg(long, default_value = "auto")]
        direction: String,
        /// The PDU has no service centre address prefix.
        #[arg(long)]
        no_smsc: bool,
    },
}

#[derive(Subcommand)]
pub enum ModemCommand {
    /// Identification, SIM, network and capabilities.
    Info,
    /// Signal strength.
    Signal,
    /// Drop and re-establish the modem connection.
    Reconnect,
    /// Permit one new SIM PIN attempt after correcting sim.conf.
    ResetPinCounter,
}

#[derive(Subcommand)]
pub enum DbCommand {
    /// Apply pending schema migrations.
    Migrate,
    /// Create and verify a backup (SQLite).
    Backup {
        /// Directory for the backup (default: paths.backups).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Check database integrity, or verify a backup file.
    Verify { file: Option<String> },
    /// Restore a SQLite backup. The daemon must be stopped.
    Restore {
        file: String,
        /// Confirm overwriting the current database.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum IntegrationCommand {
    /// List configured integrations.
    List,
    /// Send a synthetic test event through an integration.
    Test { name: String },
}

impl Cli {
    pub fn load_options(&self) -> LoadOptions {
        let mut o = LoadOptions::new(&self.config);
        o.overrides = self.overrides.clone();
        o
    }

    fn load(&self) -> Result<Config> {
        config::load(&self.load_options()).map_err(|e| anyhow!("{e}"))
    }
}

/// HTTP client for the daemon's API.
struct Api {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Api {
    fn new(cli: &Cli, cfg: &Config) -> Self {
        let base = cli.api_url.clone().unwrap_or_else(|| {
            let host = match cfg.api.bind.as_str() {
                "0.0.0.0" => "127.0.0.1".to_string(),
                "::" => "[::1]".to_string(),
                h if h.contains(':') => format!("[{h}]"),
                h => h.to_string(),
            };
            format!("http://{host}:{}", cfg.api.port)
        });
        let token = cfg
            .api
            .admin_tokens
            .iter()
            .chain(&cfg.api.tokens)
            .find(|t| !t.is_empty())
            .cloned();
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(90))
            .build()
            .expect("HTTP client");
        Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            http,
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> Result<Value> {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(|e| {
            anyhow!("cannot reach the gateway daemon at {} ({}). Is the service running? Try: systemctl status thugsred-smsgateway", self.base, e.without_url())
        })?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!(
                "{} ({})",
                v["error"]["message"].as_str().unwrap_or("request failed"),
                status
            );
        }
        Ok(v)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.call(reqwest::Method::GET, path, None, &[]).await
    }

    async fn post(&self, path: &str, body: Option<Value>) -> Result<Value> {
        self.call(reqwest::Method::POST, path, body, &[]).await
    }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn s(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn print_messages(rows: &Value) {
    println!(
        "{:<20} {:<4} {:<18} {:<18} {:<12} BODY",
        "TIME", "DIR", "FROM", "TO", "STATUS"
    );
    for m in rows.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let time = m["created_at"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(19)
            .collect::<String>();
        let dir = if m["direction"] == "inbound" {
            "in"
        } else {
            "out"
        };
        let body: String = s(&m["body"])
            .replace(['\r', '\n'], " ")
            .chars()
            .take(60)
            .collect();
        println!(
            "{:<20} {:<4} {:<18} {:<18} {:<12} {}",
            time,
            dir,
            s(&m["sender"]),
            s(&m["recipient"]),
            s(&m["status"]),
            body
        );
    }
}

fn enc(v: &str) -> String {
    percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC).to_string()
}

pub async fn main(cli: Cli) -> Result<()> {
    match &cli.command {
        Command::Run { simulate_modem } => run::run(&cli, *simulate_modem).await,
        Command::Version => {
            let v = crate::daemon::version_info();
            if cli.json {
                print_json(&v);
            } else {
                println!(
                    "THUGSred-smsGateway {} (commit {}, built {}, {})",
                    s(&v["version"]),
                    s(&v["commit"]),
                    s(&v["build_date"]),
                    s(&v["target"])
                );
                println!("https://thugs.red");
            }
            Ok(())
        }
        Command::CheckConfig { show } => match config::load(&cli.load_options()) {
            Ok(cfg) => {
                if cli.json {
                    print_json(
                        &json!({"valid": true, "warnings": cfg.warnings, "config": if *show { cfg.redacted() } else { Value::Null }}),
                    );
                } else {
                    for w in &cfg.warnings {
                        println!("warning: {w}");
                    }
                    println!("Configuration is valid ({} modem(s), {} SIM profile(s), {} integration(s), {} routing rule(s), {} contact(s)).", cfg.modems.len(), cfg.sims.len(), cfg.integration.len(), cfg.routing.len(), cfg.contacts.len());
                    println!("  main:         {}", cfg.files.main);
                    println!("  sim:          {}", cfg.files.sim);
                    println!("  integrations: {}", cfg.files.integrations);
                    if *show {
                        print_json(&cfg.redacted());
                    }
                }
                Ok(())
            }
            Err(e) => {
                if cli.json {
                    print_json(&json!({"valid": false, "errors": e.errors}));
                }
                Err(anyhow!("{e}"))
            }
        },
        Command::Doctor => doctor::run(&cli).await,
        Command::Simulator { link } => run::simulator(link.as_deref()).await,
        Command::Pdu { command } => pdu_command(&cli, command),
        Command::Db { command } => db_command(&cli, command).await,
        _ => daemon_command(&cli).await,
    }
}

fn pdu_command(cli: &Cli, command: &PduCommand) -> Result<()> {
    match command {
        PduCommand::Encode {
            to,
            message,
            binary,
            encoding,
            smsc,
            flash,
            delivery_report,
            validity_minutes,
            destination_port,
            source_port,
        } => {
            let payload = match (message, binary) {
                (Some(t), None) => Payload::Text(t.clone()),
                (None, Some(h)) => {
                    Payload::Binary(hex::decode(h.trim()).context("--binary must be hexadecimal")?)
                }
                _ => bail!("provide exactly one of --message or --binary"),
            };
            let encoding = match encoding.as_str() {
                "auto" => Encoding::Auto,
                "gsm7" => Encoding::Gsm7,
                "ucs2" => Encoding::Ucs2,
                "binary" | "8bit" => Encoding::Binary,
                other => bail!("unknown encoding {other:?}"),
            };
            let req = SubmitRequest {
                to: crate::utils::normalize_number(to),
                smsc: smsc.clone(),
                payload,
                encoding,
                allow_unicode: true,
                message_class: flash.then_some(0),
                status_report: *delivery_report,
                validity_minutes: *validity_minutes,
                protocol_identifier: 0,
                destination_port: *destination_port,
                source_port: *source_port,
                reference: 0,
                max_parts: 255,
                reject_duplicates: false,
            };
            let out = pdu::encode_submit(&req).map_err(|e| anyhow!("{e}"))?;
            if cli.json {
                print_json(&serde_json::to_value(&out)?);
            } else {
                println!(
                    "encoding: {}   parts: {}   (offline: nothing was transmitted)",
                    out.encoding.as_str(),
                    out.parts.len()
                );
                for p in &out.parts {
                    println!(
                        "part {} (AT+CMGS={}):\n{}",
                        p.sequence, p.tpdu_length, p.pdu
                    );
                }
            }
            Ok(())
        }
        PduCommand::Decode {
            pdu: arg,
            direction,
            no_smsc,
        } => {
            let text = match arg {
                Some(p) => p.clone(),
                None => {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                }
            };
            let decoded = match direction.as_str() {
                "auto" => pdu::decode_auto(&text),
                "deliver" => pdu::decode(&text, pdu::PduDirection::MobileTerminated, !no_smsc),
                "submit" => pdu::decode(&text, pdu::PduDirection::MobileOriginated, !no_smsc),
                other => bail!("unknown direction {other:?}"),
            }
            .map_err(|e| anyhow!("{e}"))?;
            print_json(&serde_json::to_value(&decoded)?);
            Ok(())
        }
    }
}

async fn db_command(cli: &Cli, command: &DbCommand) -> Result<()> {
    let cfg = cli.load()?;
    match command {
        DbCommand::Verify { file: Some(file) } => {
            crate::database::verify_sqlite_file(file)
                .await
                .map_err(|e| anyhow!("{e}"))?;
            println!("{file}: integrity check passed");
            Ok(())
        }
        DbCommand::Restore { file, yes } => {
            if cfg.database.driver != "sqlite" {
                bail!("restore supports SQLite only; restore MySQL/MariaDB dumps with the mysql client (see docs/BACKUP-RESTORE.md)");
            }
            crate::database::verify_sqlite_file(file)
                .await
                .map_err(|e| anyhow!("backup is not usable: {e}"))?;
            if !*yes {
                bail!(
                    "this replaces {} with {file}; stop the daemon first and repeat with --yes",
                    cfg.database.sqlite_path
                );
            }
            let target = &cfg.database.sqlite_path;
            if std::path::Path::new(target).exists() {
                let keep = format!(
                    "{target}.before-restore-{}",
                    chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
                );
                std::fs::rename(target, &keep)
                    .with_context(|| format!("cannot move {target} aside"))?;
                println!("previous database kept as {keep}");
            }
            for suffix in ["-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{target}{suffix}"));
            }
            std::fs::copy(file, target)
                .with_context(|| format!("cannot copy {file} to {target}"))?;
            // Give the restored file to the owner of its directory (the
            // service account) when this command runs as root.
            if let Some(meta) = std::path::Path::new(target)
                .parent()
                .and_then(|d| std::fs::metadata(d).ok())
            {
                use std::os::unix::fs::MetadataExt;
                let _ = std::os::unix::fs::chown(target, Some(meta.uid()), Some(meta.gid()));
            }
            println!("restored {file} to {target}");
            Ok(())
        }
        other => {
            let db = Db::connect(&cfg.database)
                .await
                .map_err(|e| anyhow!("{e}"))?;
            let result: Result<()> = async {
                match other {
                    DbCommand::Migrate => {
                        let applied = db.migrate().await.map_err(|e| anyhow!("{e}"))?;
                        if applied.is_empty() {
                            println!(
                                "database schema is up to date (version {})",
                                db.schema_version().await.unwrap_or(0)
                            );
                        } else {
                            println!("applied migrations: {applied:?}");
                        }
                    }
                    DbCommand::Backup { dir } => {
                        let dir = dir.clone().unwrap_or_else(|| cfg.paths.backups.clone());
                        let path = db.backup(&dir).await.map_err(|e| anyhow!("{e}"))?;
                        println!("backup written and verified: {path}");
                    }
                    DbCommand::Verify { .. } => {
                        let v = db.verify().await.map_err(|e| anyhow!("{e}"))?;
                        if cli.json {
                            print_json(&v);
                        } else {
                            println!(
                                "engine: {}  schema version: {}  result: {}",
                                s(&v["engine"]),
                                s(&v["schema_version"]),
                                if v["ok"] == true { "ok" } else { "FAILED" }
                            );
                        }
                        if v["ok"] != true {
                            bail!("database verification failed: {}", v["details"]);
                        }
                    }
                    DbCommand::Restore { .. } => {}
                }
                Ok(())
            }
            .await;
            db.close().await;
            result
        }
    }
}

/// Commands served by the running daemon.
async fn daemon_command(cli: &Cli) -> Result<()> {
    let cfg = cli.load()?;
    let api = Api::new(cli, &cfg);
    let modem_q = |m: &Option<String>| {
        m.as_ref()
            .map(|m| format!("?modem={}", enc(m)))
            .unwrap_or_default()
    };
    match &cli.command {
        Command::Status => {
            let v = api.get("/api/v1/status").await?;
            if cli.json {
                print_json(&v["data"]);
                return Ok(());
            }
            let d = &v["data"];
            println!(
                "{} {} - up {} s (instance {})",
                s(&d["daemon"]["instance_name"]),
                s(&d["daemon"]["version"]),
                s(&d["daemon"]["uptime_seconds"]),
                s(&d["daemon"]["instance_id"])
            );
            for m in d["modems"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                println!(
                    "modem {:<10} {:<13} {} {}  operator: {}  signal: {}%  SIM: {}  number: {}",
                    s(&m["id"]),
                    s(&m["state"]),
                    s(&m["manufacturer"]),
                    s(&m["model"]),
                    s(&m["operator"]),
                    s(&m["signal_percent"]),
                    s(&m["sim_state"]),
                    s(&m["mobile_number"])
                );
                if !m["hint"].is_null() {
                    println!("  hint: {}", s(&m["hint"]));
                }
            }
            println!(
                "database: {} ({})  queue: {} pending",
                if d["database"]["healthy"] == true {
                    "healthy"
                } else {
                    "UNAVAILABLE"
                },
                s(&d["database"]["driver"]),
                s(&d["queue"]["pending"])
            );
            for i in d["integrations"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                println!(
                    "integration {:<20} {:<9} {}",
                    s(&i["name"]),
                    s(&i["type"]),
                    if i["enabled"] != true {
                        "disabled"
                    } else if i["healthy"] == true {
                        "healthy"
                    } else {
                        "failing"
                    }
                );
            }
        }
        Command::Capabilities => print_json(&api.get("/api/v1/capabilities").await?["data"]),
        Command::Diagnostics => print_json(&api.get("/api/v1/diagnostics").await?["data"]),
        Command::Reload => {
            let v = api.post("/api/v1/config/reload", None).await?;
            println!("configuration reloaded");
            let restart = v["data"]["restart_required"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if !restart.is_empty() {
                println!(
                    "restart required for: {}",
                    restart.iter().map(s).collect::<Vec<_>>().join(", ")
                );
            }
        }
        Command::Ussd { command, modem } => {
            let v = api
                .post(
                    "/api/v1/ussd",
                    Some(json!({"command": command, "modem": modem})),
                )
                .await?;
            println!("{}", s(&v["data"]["response"]));
        }
        Command::Sms { command } => match command {
            SmsCommand::Send(a) => {
                let mut body = json!({"to": a.to, "message": a.message, "flash": a.flash, "delivery_report": !a.no_delivery_report});
                for (k, v) in [
                    ("modem", &a.modem),
                    ("from", &a.from),
                    ("scheduled_at", &a.at),
                ] {
                    if let Some(v) = v {
                        body[k] = json!(v);
                    }
                }
                let headers: Vec<(&str, &str)> = a
                    .idempotency_key
                    .as_deref()
                    .map(|k| vec![("Idempotency-Key", k)])
                    .unwrap_or_default();
                let v = api
                    .call(
                        reqwest::Method::POST,
                        "/api/v1/sms/send",
                        Some(body),
                        &headers,
                    )
                    .await?;
                if cli.json {
                    print_json(&v);
                } else {
                    println!(
                        "queued: {} ({} part(s), {}, status {})",
                        s(&v["message_id"]),
                        s(&v["parts"]),
                        s(&v["encoding"]),
                        s(&v["status"])
                    );
                }
            }
            SmsCommand::List { limit, direction } => {
                let dir = direction
                    .as_ref()
                    .map(|d| format!("&direction={}", enc(d)))
                    .unwrap_or_default();
                let v = api
                    .get(&format!("/api/v1/messages?limit={limit}{dir}"))
                    .await?;
                if cli.json {
                    print_json(&v["data"]);
                } else {
                    print_messages(&v["data"]);
                }
            }
            SmsCommand::Search {
                sender,
                recipient,
                text,
                limit,
            } => {
                let mut q = format!("/api/v1/messages?limit={limit}");
                for (k, v) in [
                    ("sender", sender),
                    ("recipient", recipient),
                    ("search", text),
                ] {
                    if let Some(v) = v {
                        q.push_str(&format!("&{k}={}", enc(v)));
                    }
                }
                let v = api.get(&q).await?;
                if cli.json {
                    print_json(&v["data"]);
                } else {
                    print_messages(&v["data"]);
                    println!("{} match(es)", s(&v["pagination"]["total"]));
                }
            }
        },
        Command::Modem { command, modem } => {
            match command {
                ModemCommand::Info => {
                    let v = api.get(&format!("/api/v1/modem{}", modem_q(modem))).await?;
                    let d = &v["data"];
                    if cli.json {
                        print_json(d);
                    } else {
                        println!("modem:        {} ({})", s(&d["id"]), s(&d["state"]));
                        println!(
                            "device:       {} @ {} baud",
                            s(&d["device"]),
                            s(&d["baud_rate"])
                        );
                        println!(
                            "hardware:     {} {} (firmware {}, profile {})",
                            s(&d["manufacturer"]),
                            s(&d["model"]),
                            s(&d["firmware"]),
                            s(&d["profile"])
                        );
                        println!(
                            "SIM:          {} (profile {})",
                            s(&d["sim"]["state"]),
                            s(&d["sim"]["profile"])
                        );
                        println!(
                            "network:      {} on {} ({})",
                            s(&d["network"]["registration"]),
                            s(&d["network"]["operator"]),
                            s(&d["network"]["technology"])
                        );
                        println!(
                            "signal:       {}% ({} dBm)",
                            s(&d["signal"]["percent"]),
                            s(&d["signal"]["rssi_dbm"])
                        );
                        println!(
                            "SMS storage:  {}/{} in {}",
                            s(&d["storage"]["used"]),
                            s(&d["storage"]["total"]),
                            s(&d["storage"]["name"])
                        );
                        let c = &d["capabilities"];
                        println!("capabilities: sms {}  voice {}  ussd {}  gnss {}  mms {}  recording {}", s(&c["sms_modes"]), s(&c["voice"]), s(&c["ussd"]), s(&c["gnss"]), s(&c["mms"]), s(&c["audio"]["call_recording"]));
                        if !d["last_error"].is_null() {
                            println!("last error:   {}", s(&d["last_error"]));
                        }
                        if !d["hint"].is_null() {
                            println!("hint:         {}", s(&d["hint"]));
                        }
                    }
                }
                ModemCommand::Signal => {
                    let v = api
                        .get(&format!("/api/v1/modem/signal{}", modem_q(modem)))
                        .await?;
                    let d = &v["data"];
                    if cli.json {
                        print_json(d);
                    } else {
                        println!(
                            "{}: {}% ({} dBm, raw {})",
                            s(&d["modem"]),
                            s(&d["signal"]["percent"]),
                            s(&d["signal"]["rssi_dbm"]),
                            s(&d["signal"]["raw"])
                        );
                    }
                }
                ModemCommand::Reconnect => {
                    api.post(&format!("/api/v1/modem/reconnect{}", modem_q(modem)), None)
                        .await?;
                    println!("reconnect requested");
                }
                ModemCommand::ResetPinCounter => {
                    let v = api
                        .post(
                            &format!("/api/v1/modem/reset-pin-counter{}", modem_q(modem)),
                            None,
                        )
                        .await?;
                    println!("PIN attempt counter cleared for {}", s(&v["data"]["modem"]));
                }
            }
        }
        Command::Integrations { command } => match command {
            IntegrationCommand::List => {
                let v = api.get("/api/v1/integrations").await?;
                if cli.json {
                    print_json(&v["data"]);
                } else {
                    println!(
                        "{:<24} {:<9} {:<8} {:>9} {:>8} {:>5}  EVENTS",
                        "NAME", "TYPE", "ENABLED", "DELIVERED", "PENDING", "DEAD"
                    );
                    for i in v["data"]["integrations"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                    {
                        let events = i["events"]
                            .as_array()
                            .map(|a| a.iter().map(s).collect::<Vec<_>>().join(","))
                            .unwrap_or_default();
                        println!(
                            "{:<24} {:<9} {:<8} {:>9} {:>8} {:>5}  {}",
                            s(&i["name"]),
                            s(&i["type"]),
                            s(&i["enabled"]),
                            s(&i["stats"]["delivered"]),
                            s(&i["stats"]["pending"]),
                            s(&i["stats"]["dead"]),
                            events
                        );
                    }
                }
            }
            IntegrationCommand::Test { name } => {
                let v = api
                    .post(&format!("/api/v1/integrations/{}/test", enc(name)), None)
                    .await?;
                let o = &v["data"]["outcome"];
                if cli.json {
                    print_json(&v["data"]);
                } else if o["success"] == true {
                    println!(
                        "test event delivered to {name} (HTTP {}, {} ms)",
                        s(&o["status_code"]),
                        s(&o["duration_ms"])
                    );
                }
                if o["success"] != true {
                    bail!("test delivery to {name} failed: {}", s(&o["error"]));
                }
            }
        },
        _ => unreachable!("handled by the caller"),
    }
    Ok(())
}
