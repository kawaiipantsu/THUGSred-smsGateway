//! The `run` and `simulator` commands.

use super::Cli;
use crate::config::Config;
use crate::daemon::Gateway;
use crate::modem::simulator::{pty, SimConnector, SimHandle};
use crate::modem::transport::{Connector, SerialConnector};
use crate::sms::pdu::{encode_deliver, DeliverSpec};
use anyhow::{anyhow, Result};
use std::path::Path;
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};

/// Create runtime directories that the daemon owns. Failures are reported
/// but not fatal: systemd normally provides these directories.
fn ensure_dirs(cfg: &Config) {
    let p = &cfg.paths;
    for dir in [
        &p.data,
        &p.runtime,
        &p.recordings,
        &p.attachments,
        &p.backups,
    ] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(directory = %dir, error = %e, "cannot create directory");
        }
    }
}

pub async fn run(cli: &Cli, simulate: bool) -> Result<()> {
    let options = cli.load_options();
    let cfg = crate::config::load(&options).map_err(|e| anyhow!("{e}"))?;
    let log = crate::logging::init(&cfg, true);
    for w in &cfg.warnings {
        tracing::warn!("configuration: {w}");
    }
    tracing::info!(version = crate::daemon::VERSION, commit = crate::daemon::GIT_COMMIT, instance = %cfg.general.instance_id, config = %cfg.files.main, "starting THUGSred SMS Gateway");
    ensure_dirs(&cfg);
    let api_enabled = cfg.api.enabled;

    if simulate {
        tracing::warn!(
            "running with the built-in modem simulator: no real SMS is sent or received"
        );
    }
    // One independent simulated modem per configured modem.
    let sims: std::sync::Mutex<std::collections::HashMap<String, SimHandle>> = Default::default();
    let gw = Gateway::start(cfg, options, move |id| -> Arc<dyn Connector> {
        if simulate {
            let mut map = sims.lock().unwrap_or_else(|e| e.into_inner());
            let n = map.len();
            let handle = map.entry(id.to_string()).or_insert_with(|| {
                let h = SimHandle::new();
                h.with(|s| s.iccid = format!("890000000000000000{}", n + 1));
                h
            });
            Arc::new(SimConnector(handle.clone()))
        } else {
            Arc::new(SerialConnector)
        }
    })
    .await?;
    let log = Arc::new(log);
    let log2 = log.clone();
    gw.set_level_hook(move |level| log2.set_level(level));

    let mut server = if api_enabled {
        // Bind before reporting readiness so that a port conflict is fatal.
        let cfg = gw.cfg.load_full();
        let host = if cfg.api.bind.contains(':') {
            format!("[{}]", cfg.api.bind)
        } else {
            cfg.api.bind.clone()
        };
        let listener = match tokio::net::TcpListener::bind(format!("{host}:{}", cfg.api.port)).await
        {
            Ok(l) => l,
            Err(e) => {
                gw.shutdown().await;
                return Err(anyhow!("cannot listen on {host}:{}: {e}", cfg.api.port));
            }
        };
        Some(tokio::spawn(crate::api::serve_on(gw.clone(), listener)))
    } else {
        tracing::warn!("the HTTP API and dashboard are disabled (api.enabled = false)");
        None
    };

    // systemd integration: readiness and optional watchdog.
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
    let mut watchdog_usec = 0u64;
    if sd_notify::watchdog_enabled(false, &mut watchdog_usec) && watchdog_usec > 0 {
        let gw2 = gw.clone();
        let period = std::time::Duration::from_micros(watchdog_usec / 2);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(period);
            loop {
                tick.tick().await;
                // Only report liveness while the core can still reach its database.
                if gw2.db.ping().await {
                    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
                }
            }
        });
    }

    let mut hup = signal(SignalKind::hangup())?;
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut result = Ok(());
    loop {
        tokio::select! {
            _ = hup.recv() => {
                tracing::info!("SIGHUP received: reloading configuration");
                let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Reloading]);
                match gw.reload().await {
                    Ok(restart) if !restart.is_empty() => tracing::warn!(settings = ?restart, "some changed settings need a restart to take effect"),
                    Ok(_) => tracing::info!("configuration reloaded"),
                    Err(_) => {} // already logged; the previous configuration stays active
                }
                let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
            }
            _ = term.recv() => break,
            _ = int.recv() => break,
            r = async { server.as_mut().expect("guarded").await }, if server.is_some() => {
                result = match r {
                    Ok(Ok(())) => Err(anyhow!("HTTP server stopped unexpectedly")),
                    Ok(Err(e)) => Err(e),
                    Err(e) => Err(anyhow!("HTTP server task failed: {e}")),
                };
                break;
            }
        }
    }
    tracing::info!("shutting down");
    let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
    gw.shutdown().await;
    tracing::info!("stopped");
    drop(log);
    result
}

/// Development simulator: a software modem on a pseudo-terminal, controlled
/// by simple commands on standard input.
pub async fn simulator(link: Option<&Path>) -> Result<()> {
    let handle = SimHandle::new();
    let device = pty::serve(handle.clone(), link)?;
    println!("THUGSred modem simulator");
    println!(
        "  device: {}",
        link.map(|l| l.display().to_string()).unwrap_or(device)
    );
    println!("  use it with: thugsred-smsgateway run --set modem.device=<device>");
    println!("commands:");
    println!("  sms <sender> <text>     deliver an SMS (long texts become multipart)");
    println!("  call <number>           incoming call;  hangup  ends it");
    println!("  reg <0-5>               set network registration status");
    println!("  fail <cms code> | silent | ok   outcome of the next submissions");
    println!("  power off|on            simulate an unpowered module");
    println!("  drop                    drop the serial connection");
    println!("  sent                    list submitted PDUs");
    println!("  quit");
    let mut lines =
        tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(tokio::io::stdin()));
    let mut reference = 1u16;
    while let Some(line) = lines.next_line().await? {
        let mut parts = line.trim().splitn(3, ' ');
        match (parts.next().unwrap_or(""), parts.next(), parts.next()) {
            ("sms", Some(sender), Some(text)) => {
                let mut spec = DeliverSpec::text(sender, text, chrono::Local::now().fixed_offset());
                spec.reference = reference;
                reference = reference.wrapping_add(1);
                match encode_deliver(&spec) {
                    Ok(pdus) => {
                        for p in &pdus {
                            handle.incoming_sms(p);
                        }
                        println!("delivered {} PDU(s)", pdus.len());
                    }
                    Err(e) => println!("error: {e}"),
                }
            }
            ("call", Some(number), _) => handle.ring(number),
            ("hangup", ..) => handle.end_call(),
            ("reg", Some(stat), _) => match stat.parse() {
                Ok(v) => handle.set_registration(v),
                Err(_) => println!("usage: reg <0-5>"),
            },
            ("fail", Some(code), _) => match code.parse() {
                Ok(c) => {
                    handle.with(|s| s.send_mode = crate::modem::simulator::SendMode::CmsError(c))
                }
                Err(_) => println!("usage: fail <cms error code>"),
            },
            ("silent", ..) => {
                handle.with(|s| s.send_mode = crate::modem::simulator::SendMode::Silent)
            }
            ("ok", ..) => handle.with(|s| s.send_mode = crate::modem::simulator::SendMode::Normal),
            ("power", Some("off"), _) => handle.with(|s| s.powered = false),
            ("power", Some("on"), _) => {
                handle.with(|s| s.powered = true);
                handle.raw_urc("RDY");
            }
            ("drop", ..) => handle.disconnect(),
            ("sent", ..) => {
                for p in handle.sent() {
                    println!("{p}");
                }
            }
            ("quit" | "exit", ..) => break,
            ("", ..) => {}
            _ => println!("unknown command"),
        }
    }
    Ok(())
}
