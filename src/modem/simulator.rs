//! Software modem emulating a SIMCom-style GSM module.
//!
//! Used by the automated tests (in-process stream) and by the development
//! simulator (`thugsred-smsgateway simulator`, pseudo-terminal). It answers
//! the AT commands the gateway uses, stores and lists messages, accepts PDU
//! submissions, and can inject received SMS, delivery reports, calls,
//! disconnections and failures.

use super::transport::{BoxStream, Connector};
use crate::config::ModemConfig;
use crate::sms::pdu::{self, Pdu, PduDirection};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, Notify};

/// How the simulator reacts to the next `AT+CMGS` submissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    Normal,
    /// Answer with `+CMS ERROR: <code>` after the PDU.
    CmsError(u16),
    /// Accept the PDU and then never answer (ambiguous outcome).
    Silent,
    /// Never offer the `>` prompt.
    NoPrompt,
}

pub struct SimState {
    pub powered: bool,
    pub available: bool,
    pub manufacturer: String,
    pub model: String,
    pub pin: Option<String>,
    /// READY, SIM PIN or SIM PUK
    pub pin_state: String,
    pub pin_tries_left: u8,
    pub registration: u8,
    pub csq: u8,
    pub operator: String,
    pub iccid: String,
    pub storage: BTreeMap<u32, String>,
    pub capacity: u32,
    pub sent: Vec<String>,
    pub commands: Vec<String>,
    pub next_mr: u8,
    pub send_mode: SendMode,
    /// Automatically answer submissions that request a status report.
    pub auto_delivery_report: bool,
    pub echo: bool,
    /// Write responses a few bytes at a time to exercise reassembly.
    pub fragment: bool,
    pub ussd_response: String,
    pub gnss_fix: bool,
    urc: Option<mpsc::Sender<String>>,
    kill: Arc<Notify>,
}

impl Default for SimState {
    fn default() -> Self {
        Self {
            powered: true,
            available: true,
            manufacturer: "SIMCOM_Ltd".into(),
            model: "SIMCOM_SIM868".into(),
            pin: None,
            pin_state: "READY".into(),
            pin_tries_left: 3,
            registration: 1,
            csq: 20,
            operator: "Simulated Mobile".into(),
            iccid: "8900000000000000001".into(),
            storage: BTreeMap::new(),
            capacity: 50,
            sent: Vec::new(),
            commands: Vec::new(),
            next_mr: 1,
            send_mode: SendMode::Normal,
            auto_delivery_report: true,
            echo: true,
            fragment: false,
            ussd_response: "Balance: 42.00 credits".into(),
            gnss_fix: true,
            urc: None,
            kill: Arc::new(Notify::new()),
        }
    }
}

/// Control handle for a simulated modem.
#[derive(Clone, Default)]
pub struct SimHandle {
    state: Arc<Mutex<SimState>>,
}

impl SimHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut SimState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn urc(&self, text: String) {
        self.with(|s| {
            if let Some(tx) = &s.urc {
                let _ = tx.try_send(text);
            }
        });
    }

    /// Deliver an SMS into modem storage and announce it with `+CMTI`.
    pub fn incoming_sms(&self, pdu: &str) {
        let index = self.with(|s| {
            let index = (1..=s.capacity).find(|i| !s.storage.contains_key(i))?;
            s.storage.insert(index, pdu.to_string());
            Some(index)
        });
        if let Some(i) = index {
            self.urc(format!("\r\n+CMTI: \"SM\",{i}\r\n"));
        }
    }

    /// Store an SMS without announcing it (found by the next poll).
    pub fn store_silently(&self, pdu: &str) {
        self.with(|s| {
            if let Some(index) = (1..=s.capacity).find(|i| !s.storage.contains_key(i)) {
                s.storage.insert(index, pdu.to_string());
            }
        });
    }

    /// Route an SMS directly with `+CMT`.
    pub fn incoming_direct(&self, pdu: &str) {
        self.urc(format!("\r\n+CMT: ,{}\r\n{pdu}\r\n", pdu.len() / 2));
    }

    pub fn status_report(&self, pdu: &str) {
        self.urc(format!("\r\n+CDS: {}\r\n{pdu}\r\n", pdu.len() / 2));
    }

    pub fn ring(&self, number: &str) {
        self.urc(format!(
            "\r\nRING\r\n\r\n+CLIP: \"{number}\",145,\"\",0,\"\",0\r\n"
        ));
    }

    pub fn end_call(&self) {
        self.urc("\r\nNO CARRIER\r\n".into());
    }

    pub fn raw_urc(&self, line: &str) {
        self.urc(format!("\r\n{line}\r\n"));
    }

    pub fn set_registration(&self, stat: u8) {
        self.with(|s| s.registration = stat);
        self.urc(format!("\r\n+CREG: {stat}\r\n"));
    }

    /// Drop the current connection (device removal or cable fault).
    pub fn disconnect(&self) {
        self.with(|s| s.kill.notify_waiters());
    }

    pub fn sent(&self) -> Vec<String> {
        self.with(|s| s.sent.clone())
    }

    pub fn commands(&self) -> Vec<String> {
        self.with(|s| s.commands.clone())
    }

    pub fn stored(&self) -> usize {
        self.with(|s| s.storage.len())
    }

    /// Start a session on one end of a stream pair and return the other end.
    pub fn connect(&self) -> DuplexStream {
        let (near, far) = tokio::io::duplex(8192);
        tokio::spawn(session(self.clone(), far));
        near
    }
}

/// Connector that attaches the gateway to a simulated modem.
pub struct SimConnector(pub SimHandle);

#[async_trait]
impl Connector for SimConnector {
    async fn open(&self, cfg: &ModemConfig) -> io::Result<BoxStream> {
        if !self.0.with(|s| s.available) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{}: No such file or directory", cfg.device),
            ));
        }
        Ok(Box::new(self.0.connect()))
    }
}

enum Reply {
    Lines(Vec<String>, &'static str),
    Final(String),
    Prompt,
    Silent,
}

fn ok(lines: &[&str]) -> Reply {
    Reply::Lines(lines.iter().map(|s| s.to_string()).collect(), "OK")
}

fn handle(cmd: &str, s: &mut SimState) -> (Reply, Vec<String>) {
    let upper = cmd.to_ascii_uppercase();
    let mut urcs = Vec::new();
    let locked = s.pin_state != "READY";
    let reply = match upper.as_str() {
        "AT" | "ATE1" | "AT+CMEE=1" | "AT+CMGF=0" | "AT+CLIP=1" | "AT+CREG=1" | "AT+CGREG=1"
        | "AT+CUSD=1" | "AT+CUSD=2" | "ATH" | "ATA" | "AT+CGNSPWR=1" => ok(&[]),
        "ATE0" => {
            s.echo = false;
            ok(&[])
        }
        "AT+CGMI" => ok(&[&s.manufacturer.clone()]),
        "AT+CGMM" => ok(&[&s.model.clone()]),
        "AT+CGMR" => ok(&["Revision:SIMULATOR-1.0"]),
        "AT+CGSN" => ok(&["000000000000000"]),
        "AT+CMGF=?" => ok(&["+CMGF: (0,1)"]),
        "AT+CSCS=?" => ok(&["+CSCS: (\"IRA\",\"GSM\",\"UCS2\",\"HEX\")"]),
        "AT+IPR=?" => ok(&["+IPR: (),(0,9600,19200,38400,57600,115200)"]),
        "AT+CPMS=?" => ok(&["+CPMS: (\"SM\",\"ME\"),(\"SM\",\"ME\"),(\"SM\",\"ME\")"]),
        "AT+CNMI=?" => ok(&["+CNMI: (0-3),(0-3),(0,2),(0,1),(0,1)"]),
        "AT+CLIP=?" => ok(&["+CLIP: (0,1)"]),
        "AT+CUSD=?" => ok(&["+CUSD: (0-2)"]),
        "AT+CGNSPWR=?" => ok(&["+CGNSPWR: (0-1)"]),
        "AT+CMMSINIT=?" | "AT+BTPOWER=?" => ok(&[]),
        "AT+CCID" => ok(&[&s.iccid.clone()]),
        "AT+CPIN?" => ok(&[&format!("+CPIN: {}", s.pin_state)]),
        "AT+CIMI" if !locked => ok(&["001010000000001"]),
        "AT+CNUM" if !locked => ok(&[]),
        "AT+CSCA?" if !locked => ok(&["+CSCA: \"+4500000001\",145"]),
        "AT+CSQ" => ok(&[&format!("+CSQ: {},0", s.csq)]),
        "AT+CREG?" => ok(&[&format!("+CREG: 1,{}", s.registration)]),
        "AT+COPS?" => ok(&[&format!("+COPS: 0,0,\"{}\"", s.operator)]),
        "AT+CPMS?" if !locked => {
            let (u, t) = (s.storage.len(), s.capacity);
            ok(&[&format!(
                "+CPMS: \"SM\",{u},{t},\"SM\",{u},{t},\"SM\",{u},{t}"
            )])
        }
        "AT+CMGL=4" if !locked => {
            let mut lines = Vec::new();
            for (i, p) in &s.storage {
                lines.push(format!("+CMGL: {i},0,,{}", p.len() / 2));
                lines.push(p.clone());
            }
            Reply::Lines(lines, "OK")
        }
        "AT+CGNSINF" => {
            if s.gnss_fix {
                ok(&["+CGNSINF: 1,1,20250102030405.000,55.676100,12.568300,12.500,0.00,0.0,1,,1.1,1.4,0.9,,11,7,,,40,,"])
            } else {
                ok(&["+CGNSINF: 1,0,,,,,,,0,,,,,,0,0,,,,,"])
            }
        }
        _ if upper.starts_with("AT+CPIN=") => {
            let entered = cmd[8..].trim_matches('"').to_string();
            if s.pin_state == "SIM PIN" && Some(&entered) == s.pin.as_ref() {
                s.pin_state = "READY".into();
                s.pin_tries_left = 3;
                ok(&[])
            } else {
                if s.pin_state == "SIM PIN" {
                    s.pin_tries_left = s.pin_tries_left.saturating_sub(1);
                    if s.pin_tries_left == 0 {
                        s.pin_state = "SIM PUK".into();
                    }
                }
                Reply::Final("+CME ERROR: 16".into())
            }
        }
        _ if locked
            && (upper.starts_with("AT+CMG")
                || upper.starts_with("AT+CPMS")
                || upper.starts_with("AT+CNMI")
                || upper.starts_with("AT+CSCA")) =>
        {
            Reply::Final("+CME ERROR: 11".into())
        }
        _ if upper.starts_with("AT+CPMS=") => ok(&[&format!(
            "+CPMS: {0},{1},{0},{1},{0},{1}",
            s.storage.len(),
            s.capacity
        )]),
        _ if upper.starts_with("AT+CNMI=")
            || upper.starts_with("AT+CSCA=")
            || upper.starts_with("AT+COPS=") =>
        {
            ok(&[])
        }
        _ if upper.starts_with("AT+CMGR=") => {
            match cmd[8..].parse::<u32>().ok().and_then(|i| s.storage.get(&i)) {
                Some(p) => {
                    Reply::Lines(vec![format!("+CMGR: 0,,{}", p.len() / 2), p.clone()], "OK")
                }
                None => Reply::Final("+CMS ERROR: 321".into()),
            }
        }
        _ if upper.starts_with("AT+CMGD=") => {
            if let Ok(i) = cmd[8..].split(',').next().unwrap_or("").parse::<u32>() {
                s.storage.remove(&i);
            }
            ok(&[])
        }
        _ if upper.starts_with("AT+CMGS=") => {
            if s.registration != 1 && s.registration != 5 {
                Reply::Final("+CMS ERROR: 331".into())
            } else if s.send_mode == SendMode::NoPrompt {
                Reply::Silent
            } else {
                Reply::Prompt
            }
        }
        _ if upper.starts_with("AT+CUSD=1,") => {
            urcs.push(format!("\r\n+CUSD: 0,\"{}\",15\r\n", s.ussd_response));
            ok(&[])
        }
        _ => Reply::Final("ERROR".into()),
    };
    (reply, urcs)
}

/// Result of a PDU submission, plus an optional delivery report to emit.
fn submit(pdu_hex: &str, s: &mut SimState) -> (String, Option<String>) {
    match s.send_mode {
        SendMode::CmsError(code) => return (format!("\r\n+CMS ERROR: {code}\r\n"), None),
        SendMode::Silent => return (String::new(), None),
        _ => {}
    }
    let Ok(Pdu::Submit(sub)) = pdu::decode(pdu_hex, PduDirection::MobileOriginated, true) else {
        return ("\r\n+CMS ERROR: 304\r\n".into(), None);
    };
    let mr = s.next_mr;
    s.next_mr = s.next_mr.wrapping_add(1);
    s.sent.push(pdu_hex.to_string());
    let report = (sub.status_report_request && s.auto_delivery_report)
        .then(|| {
            let now = chrono::Utc::now().fixed_offset();
            pdu::encode_status_report(
                Some("+4500000001"),
                mr,
                &sub.recipient.number,
                now,
                now,
                0x00,
            )
            .ok()
        })
        .flatten();
    (format!("\r\n+CMGS: {mr}\r\n\r\nOK\r\n"), report)
}

async fn write(stream: &mut DuplexStream, data: &str, fragment: bool) -> io::Result<()> {
    if fragment {
        for chunk in data.as_bytes().chunks(3) {
            stream.write_all(chunk).await?;
            stream.flush().await?;
            tokio::task::yield_now().await;
        }
        Ok(())
    } else {
        stream.write_all(data.as_bytes()).await
    }
}

async fn session(handle: SimHandle, mut stream: DuplexStream) {
    let (urc_tx, mut urc_rx) = mpsc::channel::<String>(256);
    let kill = handle.with(|s| {
        s.urc = Some(urc_tx);
        s.echo = true;
        s.kill.clone()
    });
    let mut line: Vec<u8> = Vec::new();
    let mut pdu_mode = false;
    let mut buf = [0u8; 512];
    loop {
        tokio::select! {
            _ = kill.notified() => break,
            Some(u) = urc_rx.recv() => {
                let (powered, fragment) = handle.with(|s| (s.powered, s.fragment));
                if powered && write(&mut stream, &u, fragment).await.is_err() {
                    break;
                }
            }
            n = stream.read(&mut buf) => {
                let n = match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if !handle.with(|s| s.powered) {
                    continue; // an unpowered module stays silent
                }
                for &b in &buf[..n] {
                    if pdu_mode {
                        match b {
                            0x1A => {
                                pdu_mode = false;
                                let text = String::from_utf8_lossy(&line).trim().to_string();
                                line.clear();
                                let (resp, report, fragment) = handle.with(|s| {
                                    let (r, rep) = submit(&text, s);
                                    (r, rep, s.fragment)
                                });
                                if write(&mut stream, &resp, fragment).await.is_err() {
                                    return;
                                }
                                if let Some(r) = report {
                                    let h = handle.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(Duration::from_millis(150)).await;
                                        h.status_report(&r);
                                    });
                                }
                            }
                            0x1B => {
                                pdu_mode = false;
                                line.clear();
                                let _ = write(&mut stream, "\r\nOK\r\n", false).await;
                            }
                            _ => line.push(b),
                        }
                        continue;
                    }
                    if b != b'\r' {
                        if b != b'\n' {
                            line.push(b);
                        }
                        continue;
                    }
                    let cmd = String::from_utf8_lossy(&line).trim().to_string();
                    line.clear();
                    if cmd.is_empty() {
                        continue;
                    }
                    let (reply, urcs, echo, fragment) = handle.with(|s| {
                        // PIN values are not kept in the command log.
                        s.commands.push(if cmd.to_ascii_uppercase().starts_with("AT+CPIN=") { "AT+CPIN=<pin>".into() } else { cmd.clone() });
                        let echo = s.echo;
                        let (r, u) = handle_cmd(&cmd, s);
                        (r, u, echo, s.fragment)
                    });
                    let mut out = String::new();
                    if echo {
                        out.push_str(&cmd);
                        out.push('\r');
                    }
                    match reply {
                        Reply::Lines(lines, fin) => {
                            for l in lines {
                                out.push_str(&format!("\r\n{l}\r\n"));
                            }
                            out.push_str(&format!("\r\n{fin}\r\n"));
                        }
                        Reply::Final(f) => out.push_str(&format!("\r\n{f}\r\n")),
                        Reply::Prompt => {
                            pdu_mode = true;
                            out.push_str("\r\n> ");
                        }
                        Reply::Silent => {}
                    }
                    for u in urcs {
                        out.push_str(&u);
                    }
                    if write(&mut stream, &out, fragment).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
    handle.with(|s| s.urc = None);
}

fn handle_cmd(cmd: &str, s: &mut SimState) -> (Reply, Vec<String>) {
    handle(cmd, s)
}

/// Pseudo-terminal front end for the development simulator.
pub mod pty {
    use super::SimHandle;
    use nix::fcntl::OFlag;
    use nix::pty::{grantpt, posix_openpt, ptsname_r, unlockpt};
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::path::Path;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Create a pseudo-terminal served by the simulator. Returns the path of
    /// the terminal device to use as `modem.device`. When `link` is given a
    /// symbolic link with that name points at the device.
    pub fn serve(handle: SimHandle, link: Option<&Path>) -> std::io::Result<String> {
        let master =
            posix_openpt(OFlag::O_RDWR | OFlag::O_NOCTTY).map_err(std::io::Error::other)?;
        grantpt(&master).map_err(std::io::Error::other)?;
        unlockpt(&master).map_err(std::io::Error::other)?;
        let name = ptsname_r(&master).map_err(std::io::Error::other)?;
        if let Some(link) = link {
            let _ = std::fs::remove_file(link);
            std::os::unix::fs::symlink(&name, link)?;
        }
        // Keep one descriptor on the terminal side open so that the master
        // does not report EIO while the gateway is not connected.
        let keepalive = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)?;
        let raw = master.into_raw_fd();
        // SAFETY: `raw` was just released from the owning `PtyMaster`, so this
        // is the unique owner of a valid descriptor.
        let mut reader = unsafe { std::fs::File::from_raw_fd(raw) };
        let mut writer = reader.try_clone()?;
        let (mut near_r, mut near_w) = tokio::io::split(handle.connect());
        let (to_sim, mut from_pty) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        let (to_pty, from_sim) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let _keep = keepalive;
            let mut buf = [0u8; 512];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => std::thread::sleep(std::time::Duration::from_millis(100)),
                    Ok(n) => {
                        if to_sim.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
                }
            }
        });
        std::thread::spawn(move || {
            while let Ok(data) = from_sim.recv() {
                let _ = writer.write_all(&data);
            }
            let _ = writer.as_raw_fd();
        });
        tokio::spawn(async move {
            while let Some(data) = from_pty.recv().await {
                if near_w.write_all(&data).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            while let Ok(n) = near_r.read(&mut buf).await {
                if n == 0 || to_pty.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Ok(name)
    }
}
