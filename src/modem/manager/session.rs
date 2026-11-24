//! The connected phase of a modem session: unsolicited notifications, health
//! checks, inbound message retrieval and request execution.

use super::*;
use crate::gnss;

/// A USSD request awaiting its `+CUSD` answer: reply channel, deadline, session id.
type PendingUssd = (oneshot::Sender<Result<String, String>>, Instant, String);

struct Loop<'a> {
    mc: &'a ModemConfig,
    at: &'a AtHandle,
    profile: ModemProfile,
    sim: Option<SimProfile>,
    sim_ready: bool,
    registered: bool,
    reg_stat: [Option<u8>; 3],
    at_failures: u32,
    ussd: Option<PendingUssd>,
    last_signal_percent: Option<u8>,
    storage_warned: bool,
}

fn decode_ussd(text: &str, dcs: Option<u8>) -> String {
    let hexish =
        text.len() >= 4 && text.len() % 4 == 0 && text.chars().all(|c| c.is_ascii_hexdigit());
    let ucs2 = dcs.map(|d| d & 0x0C == 0x08).unwrap_or(false);
    if hexish && ucs2 {
        if let Ok(bytes) = hex::decode(text) {
            return crate::sms::pdu::types::decode_ucs2(&bytes);
        }
    }
    text.to_string()
}

impl Worker {
    pub(super) async fn main_loop(
        &mut self,
        mc: &ModemConfig,
        at: &AtHandle,
        urc: &mut mpsc::Receiver<Urc>,
        init: Init,
    ) -> End {
        let mut lp = Loop {
            mc,
            at,
            profile: init.profile,
            sim: init.sim,
            sim_ready: init.sim_ready,
            registered: false,
            reg_stat: [None; 3],
            at_failures: 0,
            ussd: None,
            last_signal_percent: None,
            storage_warned: false,
        };
        if lp.sim_ready {
            self.set_state(ModemState::Registering);
        }
        let every = |secs: u64| {
            let mut i = tokio::time::interval(Duration::from_secs(secs.max(1)));
            i.set_missed_tick_behavior(MissedTickBehavior::Delay);
            i
        };
        let mut health = every(mc.healthcheck_interval_seconds);
        let mut poll = every(mc.poll_interval_seconds);
        let mut gnss_tick = every(self.deps.cfg.load().gnss.poll_interval_seconds);
        let mut config_check = every(5);
        // Registration is polled quickly until the modem is registered.
        let mut reg_tick = every(3);

        loop {
            let ussd_deadline = lp.ussd.as_ref().map(|u| u.1);
            let ussd_sleep = async {
                match ussd_deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            let step: Result<(), End> = tokio::select! {
                _ = self.shutdown.changed() => Err(End::Shutdown),
                u = urc.recv() => match u {
                    None => Err(lost("serial connection lost")),
                    Some(u) => self.on_urc(&mut lp, u).await,
                },
                req = self.rx.recv() => match req {
                    None => Err(End::Shutdown),
                    Some(r) => self.on_request(&mut lp, r).await,
                },
                _ = health.tick() => self.health_check(&mut lp).await,
                _ = reg_tick.tick(), if lp.sim_ready && !lp.registered => self.check_registration(&mut lp).await,
                _ = poll.tick(), if lp.sim_ready => self.poll_messages(&mut lp).await,
                _ = gnss_tick.tick() => self.poll_gnss(&mut lp).await,
                _ = config_check.tick() => {
                    // Apply modem setting changes from a configuration reload.
                    match self.mc() {
                        Some(new) if &new == mc => Ok(()),
                        Some(new) if !new.enabled => {
                            tracing::info!(modem = %self.id, "modem disabled by configuration");
                            Err(End::Reconnect)
                        }
                        Some(_) => {
                            tracing::info!(modem = %self.id, "modem configuration changed; reconnecting");
                            Err(End::Reconnect)
                        }
                        None => Err(End::Reconnect),
                    }
                },
                _ = ussd_sleep => {
                    if let Some((reply, _, id)) = lp.ussd.take() {
                        let _ = self.opt(at, "AT+CUSD=2", mc).await;
                        let _ = self.deps.db.finish_ussd(&id, "timeout", None).await;
                        let _ = reply.send(Err("no USSD response before the timeout".into()));
                    }
                    Ok(())
                },
            };
            if let Err(end) = step {
                if let Some((reply, _, id)) = lp.ussd.take() {
                    let _ = self.deps.db.finish_ussd(&id, "failed", None).await;
                    let _ = reply.send(Err("modem connection ended".into()));
                }
                return end;
            }
        }
    }

    fn refresh_state(&mut self, lp: &Loop<'_>) {
        if !lp.sim_ready {
            return; // SIM_LOCKED / DEGRADED was set by the SIM handling
        }
        self.set_state(if lp.registered {
            ModemState::Ready
        } else {
            ModemState::Registering
        });
    }

    async fn set_registration(&mut self, lp: &mut Loop<'_>, domain: Domain, stat: u8) {
        let slot = match domain {
            Domain::Circuit => 0,
            Domain::Gprs => 1,
            Domain::Eps => 2,
        };
        lp.reg_stat[slot] = Some(stat);
        // SMS needs circuit-switched or EPS registration; GPRS alone does not carry it.
        let best = [lp.reg_stat[0], lp.reg_stat[2]]
            .into_iter()
            .flatten()
            .find(|s| parser::is_registered(*s))
            .or(lp.reg_stat[0])
            .or(lp.reg_stat[2])
            .unwrap_or(stat);
        let now = parser::is_registered(best);
        let name = parser::registration_name(best).to_string();
        self.update(|s| {
            s.network.registration = name.clone();
            s.network.registered = now;
        });
        if now != lp.registered {
            lp.registered = now;
            if now {
                // Operator name is available once registered.
                if let Ok(Some(lines)) = self.opt(lp.at, "AT+COPS?", lp.mc).await {
                    if let Some((oper, act)) = parser::parse_cops(&lines) {
                        let tech = act.map(str::to_string).or_else(|| {
                            (lp.profile.network_technologies.len() <= 2)
                                .then(|| lp.profile.network_technologies.first().cloned())
                                .flatten()
                        });
                        self.update(|s| {
                            s.network.operator = Some(oper);
                            s.network.technology = tech;
                        });
                    }
                }
                let n = self.status.borrow().network.clone();
                self.emit("network.registered", json!({"registration": n.registration, "operator": n.operator, "technology": n.technology})).await;
            } else {
                self.emit("network.lost", json!({"registration": name}))
                    .await;
            }
        }
        self.refresh_state(lp);
    }

    async fn check_registration(&mut self, lp: &mut Loop<'_>) -> Result<(), End> {
        for (cmd, prefix, domain) in [
            ("AT+CREG?", "+CREG:", Domain::Circuit),
            ("AT+CEREG?", "+CEREG:", Domain::Eps),
        ] {
            if domain == Domain::Eps
                && !self
                    .status
                    .borrow()
                    .capabilities
                    .network_technologies
                    .iter()
                    .any(|t| t == "LTE")
            {
                continue;
            }
            if let Some(lines) = self.opt(lp.at, cmd, lp.mc).await? {
                if let Some(stat) = parser::parse_reg(&lines, prefix) {
                    self.set_registration(lp, domain, stat).await;
                }
            }
        }
        Ok(())
    }

    async fn health_check(&mut self, lp: &mut Loop<'_>) -> Result<(), End> {
        match self.q(lp.at, "AT", lp.mc).await {
            Ok(_) => {
                lp.at_failures = 0;
                self.update(|s| s.last_seen_at = Some(utils::now()));
            }
            Err(AtError::Closed) => return Err(lost("serial connection closed")),
            Err(_) => {
                lp.at_failures += 1;
                if lp.at_failures >= 3 {
                    return Err(End::Failed {
                        reason: "modem stopped responding to AT commands".into(),
                        hint: Some(NO_RESPONSE.to_string()),
                        quick_retry: true,
                    });
                }
                return Ok(());
            }
        }
        if !lp.sim_ready {
            // Re-evaluate a locked or missing SIM without re-entering a PIN
            // that was already rejected (the attempt counter prevents that).
            let iccid = self.status.borrow().sim.iccid.clone();
            if self
                .unlock_sim(lp.mc, lp.at, lp.sim.clone().as_ref(), iccid.as_deref())
                .await?
            {
                self.configure(lp.mc, lp.at, &lp.profile.clone(), lp.sim.clone().as_ref())
                    .await?;
                lp.sim_ready = true;
                self.update(|s| {
                    s.last_error = None;
                    s.hint = None;
                });
                self.set_state(ModemState::Registering);
            }
            return Ok(());
        }
        if let Some(lines) = self.opt(lp.at, "AT+CSQ", lp.mc).await? {
            if let Some((raw, dbm, percent)) = parser::parse_csq(&lines) {
                self.update(|s| {
                    s.signal = SignalStatus {
                        raw: Some(raw),
                        rssi_dbm: dbm,
                        percent,
                    }
                });
                if let Some(d) = dbm {
                    self.deps.metrics.set(
                        "smsgw_modem_signal_dbm",
                        &[("modem", &self.id)],
                        d as f64,
                    );
                }
                let changed = match (lp.last_signal_percent, percent) {
                    (Some(a), Some(b)) => a.abs_diff(b) >= 15,
                    (None, Some(_)) => true,
                    _ => false,
                };
                if changed {
                    lp.last_signal_percent = percent;
                    self.emit(
                        "modem.signal_changed",
                        json!({"signal_percent": percent, "rssi_dbm": dbm}),
                    )
                    .await;
                }
            }
        }
        self.check_registration(lp).await?;
        let st = self.status.borrow().clone();
        let _ = self
            .deps
            .db
            .insert_health(
                &self.id,
                st.state.as_str(),
                st.signal.rssi_dbm.map(i64::from),
                st.signal.percent.map(i64::from),
                Some(&st.network.registration),
                st.network.operator.as_deref(),
                st.network.technology.as_deref(),
            )
            .await;
        Ok(())
    }

    /// Fetch messages that are stored on the modem or SIM. This catches
    /// anything that arrived while the gateway was not listening.
    async fn poll_messages(&mut self, lp: &mut Loop<'_>) -> Result<(), End> {
        let cfg = self.deps.cfg.load_full();
        if let Some(lines) = self.opt(lp.at, "AT+CPMS?", lp.mc).await? {
            if let Some((name, used, total)) = parser::parse_cpms(&lines) {
                self.update(|s| {
                    s.storage = StorageStatus {
                        name: Some(name),
                        used: Some(used),
                        total: Some(total),
                    }
                });
                let nearly_full = total > 0 && used * 10 >= total * 9;
                if nearly_full && !lp.storage_warned {
                    self.emit("modem.error", json!({"error": format!("modem SMS storage is nearly full ({used}/{total})")})).await;
                }
                lp.storage_warned = nearly_full;
                if used == 0 {
                    return Ok(());
                }
            }
        }
        if !cfg.sms.enabled || !cfg.sms.receive_enabled {
            return Ok(());
        }
        let Some(lines) = self.opt(lp.at, "AT+CMGL=4", lp.mc).await? else {
            return Ok(());
        };
        for (index, pdu) in parser::parse_cmgl(&lines) {
            self.ingest(lp, &pdu, Some(index)).await?;
        }
        Ok(())
    }

    /// Hand a PDU to the sink and delete it from the modem once persisted.
    async fn ingest(&mut self, lp: &Loop<'_>, pdu: &str, index: Option<u32>) -> Result<(), End> {
        let sim_id = self.status.borrow().sim.profile.clone();
        let persisted = self
            .deps
            .sink
            .on_pdu(&self.id, sim_id.as_deref(), pdu)
            .await;
        if let (true, Some(i)) = (persisted, index) {
            if self.deps.cfg.load().sms.delete_from_modem_after_persist {
                self.opt(lp.at, &format!("AT+CMGD={i}"), lp.mc).await?;
            }
        }
        Ok(())
    }

    async fn on_urc(&mut self, lp: &mut Loop<'_>, urc: Urc) -> Result<(), End> {
        let receive = {
            let c = self.deps.cfg.load();
            c.sms.enabled && c.sms.receive_enabled
        };
        match urc {
            Urc::MessageStored { index, .. } | Urc::ReportStored { index, .. } if receive => {
                if let Some(lines) = self.opt(lp.at, &format!("AT+CMGR={index}"), lp.mc).await? {
                    if let Some(pdu) = parser::parse_cmgr(&lines) {
                        self.ingest(lp, &pdu, Some(index)).await?;
                    }
                }
            }
            Urc::Message { pdu } | Urc::StatusReport { pdu } if receive => {
                self.ingest(lp, &pdu, None).await?
            }
            Urc::MessageStored { .. }
            | Urc::ReportStored { .. }
            | Urc::Message { .. }
            | Urc::StatusReport { .. } => {}
            Urc::Registration { domain, stat } => self.set_registration(lp, domain, stat).await,
            Urc::Ring => self.call_signal(CallSignalKind::Ring),
            Urc::CallerId { number } => self.call_signal(CallSignalKind::CallerId(number)),
            Urc::CallEnded { reason } => self.call_signal(CallSignalKind::Ended(reason)),
            Urc::Ussd { status, text, dcs } => {
                let text = text.map(|t| decode_ussd(&t, dcs));
                if status == 1 {
                    // The network expects a reply; sessions are not interactive here.
                    self.opt(lp.at, "AT+CUSD=2", lp.mc).await?;
                }
                match lp.ussd.take() {
                    Some((reply, _, id)) => {
                        let result = match (&text, status) {
                            (Some(t), 0 | 1) => Ok(t.clone()),
                            (_, s) => Err(format!("USSD request ended with status {s}")),
                        };
                        let _ = self
                            .deps
                            .db
                            .finish_ussd(
                                &id,
                                if result.is_ok() {
                                    "completed"
                                } else {
                                    "failed"
                                },
                                text.as_deref(),
                            )
                            .await;
                        let _ = reply.send(result);
                    }
                    // Network-initiated USSD is recorded and reported, never acted on.
                    None => {
                        if let Some(t) = text {
                            self.emit(
                                "ussd.response",
                                json!({"ussd": {"code": null, "response": t, "unsolicited": true}}),
                            )
                            .await;
                        }
                    }
                }
            }
            Urc::SimState(state) if state != "READY" && lp.sim_ready => {
                return Err(End::Failed {
                    reason: format!("SIM became unavailable ({state})"),
                    hint: Some(
                        "The SIM was removed or reset; the gateway re-initialises the modem."
                            .into(),
                    ),
                    quick_retry: true,
                });
            }
            Urc::SimState(_) => {}
            Urc::Booted => {
                return Err(End::Failed {
                    reason: "modem restarted".into(),
                    hint: None,
                    quick_retry: true,
                })
            }
            Urc::PowerDown(what) => {
                return Err(End::Failed {
                    reason: format!("modem powered down ({what})"),
                    hint: Some(NO_RESPONSE.to_string()),
                    quick_retry: true,
                });
            }
            Urc::Other(line) => {
                tracing::debug!(modem = %self.id, line = %line, "unsolicited notification")
            }
        }
        Ok(())
    }

    fn call_signal(&self, kind: CallSignalKind) {
        if self
            .deps
            .calls
            .try_send(CallSignal {
                modem_id: self.id.clone(),
                kind,
            })
            .is_err()
        {
            tracing::warn!(modem = %self.id, "call event queue full; call notification dropped");
        }
    }

    async fn on_request(&mut self, lp: &mut Loop<'_>, req: Req) -> Result<(), End> {
        let state = self.status.borrow().state;
        match req {
            Req::Reconnect => return Err(End::Reconnect),
            Req::SendPdu {
                tpdu_len,
                pdu,
                reply,
            } => {
                let roaming = self.status.borrow().network.registration == "roaming";
                let result = if state != ModemState::Ready {
                    Err(SendError::NotReady(format!(
                        "modem state is {}",
                        state.as_str()
                    )))
                } else if roaming
                    && !lp
                        .sim
                        .as_ref()
                        .map(|s| s.network.roaming_enabled)
                        .unwrap_or(false)
                {
                    Err(SendError::NotReady(
                        "SIM is roaming and network.roaming_enabled is false in sim.conf".into(),
                    ))
                } else {
                    let cmd = AtCommand {
                        payload: Some(pdu),
                        ..AtCommand::new(
                            format!("AT+CMGS={tpdu_len}"),
                            Duration::from_secs(lp.mc.message_timeout_seconds),
                        )
                    };
                    match lp.at.run(cmd).await {
                        Ok(lines) => Ok(parser::parse_cmgs(&lines).unwrap_or(0)),
                        Err(AtError::Rejected(e)) => Err(SendError::Rejected(e)),
                        Err(AtError::Timeout { ambiguous: true }) => Err(SendError::Ambiguous(
                            "no result after the PDU was handed to the modem".into(),
                        )),
                        Err(AtError::Timeout { ambiguous: false }) => Err(SendError::Rejected(
                            "modem did not offer a data prompt".into(),
                        )),
                        Err(AtError::Closed) => Err(SendError::Rejected(
                            "serial connection closed before submission".into(),
                        )),
                    }
                };
                let _ = reply.send(result);
            }
            Req::Ussd { code, reply } => {
                if !matches!(state, ModemState::Ready) {
                    let _ = reply.send(Err(format!("modem state is {}", state.as_str())));
                } else if lp.ussd.is_some() {
                    let _ = reply.send(Err("another USSD request is in progress".into()));
                } else {
                    let id = utils::new_id();
                    let _ = self.deps.db.insert_ussd(&id, &self.id, &code).await;
                    match self
                        .q(lp.at, &format!("AT+CUSD=1,\"{code}\",15"), lp.mc)
                        .await
                    {
                        Ok(_) => {
                            let timeout = self.deps.cfg.load().ussd.timeout_seconds;
                            lp.ussd =
                                Some((reply, Instant::now() + Duration::from_secs(timeout), id));
                        }
                        Err(e) => {
                            let _ = self.deps.db.finish_ussd(&id, "failed", None).await;
                            let _ = reply.send(Err(format!("USSD request rejected: {e}")));
                        }
                    }
                }
            }
            Req::RawAt { command, reply } => {
                let r = match self.q(lp.at, &command, lp.mc).await {
                    Ok(lines) => Ok(lines),
                    Err(AtError::Closed) => {
                        let _ = reply.send(Err("serial connection closed".into()));
                        return Err(lost("serial connection closed"));
                    }
                    Err(e) => Err(e.to_string()),
                };
                let _ = reply.send(r);
            }
            Req::Answer { reply } => {
                let r = self
                    .q(lp.at, "ATA", lp.mc)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string());
                let _ = reply.send(r);
            }
            Req::Hangup { reply } => {
                let r = self
                    .q(lp.at, "ATH", lp.mc)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string());
                let _ = reply.send(r);
            }
        }
        Ok(())
    }

    async fn poll_gnss(&mut self, lp: &mut Loop<'_>) -> Result<(), End> {
        let cfg = self.deps.cfg.load_full();
        let here = cfg.gnss.enabled
            && (cfg.gnss.modem == self.id
                || (cfg.gnss.modem.is_empty()
                    && cfg.modems.first().map(|m| m.id == self.id).unwrap_or(false)));
        let Some((_, query)) = gnss::commands(&lp.profile.gnss.kind).filter(|_| here) else {
            return Ok(());
        };
        let Some(lines) = self.opt(lp.at, query, lp.mc).await? else {
            return Ok(());
        };
        let Some(fix) = gnss::parse(&lp.profile.gnss.kind, &lines) else {
            return Ok(());
        };
        if fix.fix {
            if cfg.gnss.store_location_history {
                if let (Some(lat), Some(lon)) = (fix.latitude, fix.longitude) {
                    let _ = self
                        .deps
                        .db
                        .insert_gnss_fix(
                            &self.id,
                            lat,
                            lon,
                            fix.altitude_m,
                            fix.speed_kmh,
                            fix.heading_deg,
                            fix.satellites,
                            fix.utc_time.clone(),
                        )
                        .await;
                }
            }
            let first = self.status.borrow().gnss_last_fix_at.is_none();
            self.update(|s| s.gnss_last_fix_at = Some(utils::now()));
            if first {
                // Coordinates are deliberately not part of the event: location
                // is never forwarded to integrations.
                self.emit("gnss.fix", json!({"satellites": fix.satellites}))
                    .await;
            }
        }
        self.update(|s| s.gnss = Some(fix));
        Ok(())
    }
}
