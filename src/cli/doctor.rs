//! `doctor`: checks the installation and prints actionable advice.

use super::{Api, Cli};
use crate::config::{self, Config};
use crate::modem::actor;
use crate::modem::parser;
use crate::modem::transport::{Connector, SerialConnector};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::time::Duration;

struct Report {
    rows: Vec<(&'static str, String, String, Option<String>)>,
}

impl Report {
    fn add(
        &mut self,
        level: &'static str,
        check: impl Into<String>,
        detail: impl Into<String>,
        advice: Option<String>,
    ) {
        self.rows.push((level, check.into(), detail.into(), advice));
    }
    fn ok(&mut self, check: impl Into<String>, detail: impl Into<String>) {
        self.add("ok", check, detail, None);
    }
    fn warn(
        &mut self,
        check: impl Into<String>,
        detail: impl Into<String>,
        advice: impl Into<String>,
    ) {
        self.add("warn", check, detail, Some(advice.into()));
    }
    fn fail(
        &mut self,
        check: impl Into<String>,
        detail: impl Into<String>,
        advice: impl Into<String>,
    ) {
        self.add("fail", check, detail, Some(advice.into()));
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn writable(path: &str) -> bool {
    nix::unistd::access(
        path,
        nix::unistd::AccessFlags::W_OK | nix::unistd::AccessFlags::X_OK,
    )
    .is_ok()
}

fn check_dirs(r: &mut Report, cfg: &Config) {
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .ok()
        .flatten()
        .map(|u| u.name)
        .unwrap_or_else(|| "?".into());
    let dirs = [
        ("data", cfg.paths.data.clone()),
        (
            "logs",
            crate::logging::log_dir(&cfg.logging.system_log)
                .display()
                .to_string(),
        ),
        ("recordings", cfg.paths.recordings.clone()),
        ("backups", cfg.paths.backups.clone()),
        ("attachments", cfg.paths.attachments.clone()),
    ];
    for (name, dir) in dirs {
        let check = format!("directory: {name}");
        if !Path::new(&dir).is_dir() {
            r.warn(
                check,
                format!("{dir} does not exist"),
                "Run `sudo make install`, or create it and give it to the service user.",
            );
        } else if !writable(&dir) {
            // Running doctor as another user than the service user is normal.
            r.warn(check, format!("{dir} is not writable by {user}"), "Fine if the service user (thugsred-smsgateway) owns it; otherwise fix ownership with chown.");
        } else {
            r.ok(check, dir);
        }
    }
}

/// Things that commonly fight over a serial port.
fn check_conflicts(r: &mut Report, device: &str) {
    let real = std::fs::canonicalize(device).ok();
    let tty = real
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    if tty.is_empty() {
        return;
    }
    if command_output(
        "systemctl",
        &["is-active", &format!("serial-getty@{tty}.service")],
    )
    .as_deref()
        == Some("active")
    {
        r.fail(format!("serial conflict: {tty}"), "a login console (serial-getty) is running on this port", format!("Disable it: sudo systemctl disable --now serial-getty@{tty}.service (on a Raspberry Pi also disable the serial console in raspi-config)."));
    }
    if std::fs::read_to_string("/proc/cmdline")
        .map(|c| c.contains(&format!("console={tty}")))
        .unwrap_or(false)
    {
        r.fail(format!("serial conflict: {tty}"), "the kernel console is attached to this port", "Remove the console= entry for this port from the kernel command line (raspi-config: Interface Options > Serial Port).");
    }
    if command_output("systemctl", &["is-active", "ModemManager.service"]).as_deref()
        == Some("active")
    {
        r.warn("serial conflict: ModemManager", "ModemManager is running and may probe serial modems", "Install the provided udev rule (scripts/99-thugsred-smsgateway.rules) so ModemManager ignores the gateway's modem, or stop ModemManager if nothing else needs it.");
    }
}

fn check_device(r: &mut Report, id: &str, device: &str) -> bool {
    let check = format!("serial device: {id}");
    let meta = match std::fs::metadata(device) {
        Ok(m) => m,
        Err(_) => {
            r.fail(check, format!("{device} does not exist"), "Attach the module and check modem.device. For USB adapters, stable names are listed in /dev/serial/by-path/.");
            return false;
        }
    };
    if !meta.file_type().is_char_device() {
        r.fail(
            check,
            format!("{device} is not a character device"),
            "Point modem.device at a serial port such as /dev/ttyS0 or /dev/ttyUSB0.",
        );
        return false;
    }
    let group = nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(meta.gid()))
        .ok()
        .flatten()
        .map(|g| g.name)
        .unwrap_or_else(|| meta.gid().to_string());
    let detail = format!("{device} (group {group}, mode {:o})", meta.mode() & 0o777);
    let service_ok = command_output("id", &["-nG", "thugsred-smsgateway"])
        .map(|g| g.split_whitespace().any(|x| x == group))
        .unwrap_or(false);
    if nix::unistd::access(
        device,
        nix::unistd::AccessFlags::R_OK | nix::unistd::AccessFlags::W_OK,
    )
    .is_err()
        && !service_ok
    {
        r.fail(check, detail, format!("Neither the current user nor the service user can open it. Add the service user to the device group: sudo usermod -aG {group} thugsred-smsgateway"));
        return false;
    }
    r.ok(check, detail);
    check_conflicts(r, device);
    true
}

/// Read-only probe used when the daemon is not running.
async fn probe(r: &mut Report, cfg: &Config, id: &str) {
    let Some(mc) = cfg.modem(id) else { return };
    let stream = match SerialConnector.open(mc).await {
        Ok(s) => s,
        Err(e) => {
            r.fail(
                format!("modem probe: {id}"),
                format!("cannot open {}: {e}", mc.device),
                "Check permissions and that no other program uses the port.",
            );
            return;
        }
    };
    let (at, _urc) = actor::spawn(stream, id.to_string());
    let t = Duration::from_millis(1500);
    let mut alive = false;
    for _ in 0..4 {
        if at.at("AT", t).await.is_ok() {
            alive = true;
            break;
        }
    }
    if !alive {
        r.fail(
            format!("modem probe: {id}"),
            format!("no answer to AT on {} at {} baud", mc.device, mc.baud_rate),
            "The module is probably powered off. Hold its PWRKEY button for about two seconds until the NET LED blinks, then run doctor again. Also check the baud rate and UART jumpers.",
        );
        return;
    }
    let info = |l: Result<Vec<String>, _>, c: &str| {
        l.ok()
            .and_then(|l| parser::info_line(&l, c))
            .unwrap_or_else(|| "?".into())
    };
    let man = info(at.at("AT+CGMI", t).await, "AT+CGMI");
    let model = info(at.at("AT+CGMM", t).await, "AT+CGMM");
    r.ok(format!("modem probe: {id}"), format!("{man} {model}"));
    match at.at("AT+CPIN?", t).await {
        Ok(l) if l.iter().any(|x| x.contains("READY")) => r.ok(format!("SIM: {id}"), "ready"),
        Ok(l) => r.warn(format!("SIM: {id}"), l.join(" "), "Configure the SIM PIN in sim.conf, or unblock the SIM in a phone if it asks for a PUK."),
        Err(e) => r.fail(format!("SIM: {id}"), e.to_string(), "Insert a SIM card (with the module powered off) and check that it is seated."),
    }
    if let Ok(l) = at.at("AT+CREG?", t).await {
        match parser::parse_reg(&l, "+CREG:") {
            Some(s) if parser::is_registered(s) => r.ok(format!("network: {id}"), parser::registration_name(s)),
            Some(s) => r.warn(format!("network: {id}"), parser::registration_name(s), "Check the antenna, coverage, and that the SIM is active. 2G-only modules need an operator that still runs a GSM network."),
            None => {}
        }
    }
    if let Ok(l) = at.at("AT+CSQ", t).await {
        if let Some((_, dbm, pct)) = parser::parse_csq(&l) {
            match pct {
                Some(p) if p < 30 => r.warn(
                    format!("signal: {id}"),
                    format!("{p}% ({} dBm)", dbm.unwrap_or(0)),
                    "Weak signal: reposition the antenna.",
                ),
                Some(p) => r.ok(
                    format!("signal: {id}"),
                    format!("{p}% ({} dBm)", dbm.unwrap_or(0)),
                ),
                None => r.warn(
                    format!("signal: {id}"),
                    "unknown",
                    "The modem reports no signal measurement; check the antenna.",
                ),
            }
        }
    }
}

pub async fn run(cli: &Cli) -> Result<()> {
    let mut r = Report { rows: Vec::new() };

    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix("PRETTY_NAME=")
                    .map(|v| v.trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| std::env::consts::OS.to_string());
    r.ok("operating system", os);
    match std::env::consts::ARCH {
        a @ ("aarch64" | "x86_64") => r.ok("architecture", a),
        a => r.warn("architecture", a, "Only aarch64 and x86_64 are tested."),
    }

    let cfg = match config::load(&cli.load_options()) {
        Ok(c) => {
            if c.warnings.is_empty() {
                r.ok("configuration", format!("{} is valid", c.files.main));
            } else {
                for w in &c.warnings {
                    r.warn(
                        "configuration",
                        w.clone(),
                        "Review the setting; see docs/CONFIGURATION.md.",
                    );
                }
            }
            Some(c)
        }
        Err(e) => {
            r.fail(
                "configuration",
                e.errors.join("; "),
                "Fix the listed fields and run `thugsred-smsgateway check-config`.",
            );
            None
        }
    };

    if let Some(cfg) = &cfg {
        check_dirs(&mut r, cfg);
        match crate::metrics::disk_usage(if Path::new(&cfg.paths.data).exists() {
            &cfg.paths.data
        } else {
            "/"
        }) {
            Some((free, total)) if free < 500 * 1024 * 1024 => r.warn(
                "disk space",
                format!("{} MiB free of {} MiB", free >> 20, total >> 20),
                "Free up space or lower the retention settings.",
            ),
            Some((free, total)) => r.ok(
                "disk space",
                format!("{} MiB free of {} MiB", free >> 20, total >> 20),
            ),
            None => r.warn(
                "disk space",
                "unknown",
                "Could not read filesystem statistics.",
            ),
        }

        match crate::database::Db::connect(&cfg.database).await {
            Ok(db) => {
                match db.schema_version().await {
                    Ok(v) if v == db.latest_version() => r.ok("database", format!("{} reachable, schema version {v}", cfg.database.driver)),
                    Ok(v) => r.warn("database", format!("schema version {v}, expected {}", db.latest_version()), "Run `thugsred-smsgateway db migrate` (or start the daemon with database.auto_migrate = true)."),
                    Err(_) => r.warn("database", "reachable but not initialised", "Run `thugsred-smsgateway db migrate`."),
                }
                db.close().await;
            }
            Err(e) => r.fail(
                "database",
                e.to_string(),
                "Check the database settings, credentials and that the directory is writable.",
            ),
        }

        let api = Api::new(cli, cfg);
        let status = api.get("/api/v1/status").await;
        match &status {
            Ok(_) => r.ok("API", format!("daemon answers at {}", api.base)),
            Err(e) => r.warn(
                "API",
                e.to_string(),
                "Start the service: sudo systemctl start thugsred-smsgateway",
            ),
        }

        for p in cfg.providers.iter().filter(|p| p.enabled) {
            let check = format!("provider {}", p.name);
            if p.callback_secret.is_empty() {
                r.warn(check, format!("{} route configured, sender {:?}; no callback_secret", p.kind, p.default_sender), "Messages sent through this provider stay `submitted`. Set callback_secret and register the callback URL with the provider to receive delivery states (docs/PROVIDERS.md).");
            } else {
                r.ok(
                    check,
                    format!(
                        "{} route configured, sender {:?}, delivery callbacks enabled",
                        p.kind, p.default_sender
                    ),
                );
            }
        }

        for m in cfg.modems.iter().filter(|m| m.enabled) {
            let present = check_device(&mut r, &m.id, &m.device);
            match &status {
                Ok(v) => {
                    // The daemon owns the port: ask it instead of opening the device.
                    let st = v["data"]["modems"]
                        .as_array()
                        .and_then(|a| a.iter().find(|x| x["id"] == m.id.as_str()))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let state = st["state"].as_str().unwrap_or("unknown");
                    let detail = format!(
                        "{state}: {} {}",
                        st["manufacturer"].as_str().unwrap_or("?"),
                        st["model"].as_str().unwrap_or("")
                    );
                    let hint = st["hint"]
                        .as_str()
                        .or(st["last_error"].as_str())
                        .unwrap_or("See `thugsred-smsgateway modem info`.")
                        .to_string();
                    if state == "READY" {
                        r.ok(format!("modem: {}", m.id), detail);
                        r.ok(
                            format!("SIM: {}", m.id),
                            st["sim_state"].as_str().unwrap_or("?"),
                        );
                        r.ok(
                            format!("network: {}", m.id),
                            format!(
                                "{} on {} (signal {}%)",
                                st["registration"].as_str().unwrap_or("?"),
                                st["operator"].as_str().unwrap_or("?"),
                                st["signal_percent"]
                            ),
                        );
                    } else {
                        r.fail(format!("modem: {}", m.id), detail, hint);
                    }
                }
                Err(_) if present => probe(&mut r, cfg, &m.id).await,
                Err(_) => {}
            }
        }
        if let Ok(v) = api.get("/api/v1/capabilities").await {
            let modems = v["data"]["modems"].as_array().cloned().unwrap_or_default();
            // Capabilities are only known for modems that have been identified.
            for m in modems.iter().filter(|m| !m["model"].is_null()) {
                let c = &m["capabilities"];
                let text = |v: &Value| v.as_str().unwrap_or("unknown").to_string();
                let modes = c["sms_modes"]
                    .as_array()
                    .map(|a| a.iter().map(text).collect::<Vec<_>>().join("+"))
                    .unwrap_or_default();
                r.ok(
                    format!("capabilities: {}", m["id"].as_str().unwrap_or("?")),
                    format!(
                        "sms {modes} · voice {} · ussd {} · gnss {} · mms {} · call recording {}",
                        text(&c["voice"]),
                        text(&c["ussd"]),
                        text(&c["gnss"]),
                        text(&c["mms"]),
                        text(&c["audio"]["call_recording"])
                    ),
                );
            }
        }
    }

    match (
        command_output("systemctl", &["is-enabled", "thugsred-smsgateway.service"]),
        command_output("systemctl", &["is-active", "thugsred-smsgateway.service"]),
    ) {
        (Some(en), Some(act)) if act == "active" => r.ok("systemd service", format!("{act}, {en}")),
        (Some(en), Some(act)) if !en.is_empty() => r.warn(
            "systemd service",
            format!("{act}, {en}"),
            "sudo systemctl enable --now thugsred-smsgateway",
        ),
        _ => r.warn(
            "systemd service",
            "not installed",
            "Run `sudo make install` to install the unit file.",
        ),
    }

    let fails = r.rows.iter().filter(|x| x.0 == "fail").count();
    let warns = r.rows.iter().filter(|x| x.0 == "warn").count();
    if cli.json {
        let rows: Vec<Value> = r
            .rows
            .iter()
            .map(|(l, c, d, a)| json!({"level": l, "check": c, "detail": d, "advice": a}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"checks": rows, "failures": fails, "warnings": warns})
            )?
        );
    } else {
        println!("THUGSred SMS Gateway doctor\n");
        for (level, check, detail, advice) in &r.rows {
            let tag = match *level {
                "ok" => "[ OK ]",
                "warn" => "[WARN]",
                _ => "[FAIL]",
            };
            println!("{tag} {check}: {detail}");
            if let Some(a) = advice {
                println!("       -> {a}");
            }
        }
        println!(
            "\n{} check(s), {warns} warning(s), {fails} failure(s)",
            r.rows.len()
        );
    }
    if fails > 0 {
        bail!("{fails} check(s) failed");
    }
    Ok(())
}
