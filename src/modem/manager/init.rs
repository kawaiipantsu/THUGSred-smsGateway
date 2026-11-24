//! Modem initialisation: handshake, identification, capability detection,
//! SIM unlock and SMS configuration.

use super::*;

fn support(ok: bool) -> String {
    if ok { "supported" } else { "unsupported" }.to_string()
}

/// ALSA capture devices on the host, from /proc/asound/pcm.
pub(crate) fn host_capture_devices() -> Vec<String> {
    std::fs::read_to_string("/proc/asound/pcm")
        .map(|t| {
            t.lines()
                .filter(|l| l.contains("capture"))
                .filter_map(|l| {
                    let (id, rest) = l.split_once(':')?;
                    let (card, dev) = id.split_once('-')?;
                    let name = rest.split(':').next().unwrap_or("").trim();
                    Some(format!(
                        "hw:{},{} ({name})",
                        card.trim_start_matches('0').parse::<u32>().unwrap_or(0),
                        dev.parse::<u32>().unwrap_or(0)
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Worker {
    pub(super) async fn initialise(
        &mut self,
        mc: &ModemConfig,
        at: &AtHandle,
    ) -> Result<Init, End> {
        self.set_state(ModemState::Initializing);
        // Handshake. Autobauding modems need a few "AT"s to lock on.
        let mut alive = false;
        for _ in 0..5 {
            match at.at("AT", Duration::from_millis(1200)).await {
                Ok(_) => {
                    alive = true;
                    break;
                }
                Err(AtError::Closed) => {
                    return Err(lost("serial connection closed during handshake"))
                }
                Err(_) => {}
            }
        }
        if !alive {
            return Err(End::Failed {
                reason: "no response to AT commands (module powered off?)".into(),
                hint: Some(NO_RESPONSE.to_string()),
                quick_retry: true,
            });
        }
        self.opt(at, "ATE0", mc).await?;
        self.opt(at, "AT+CMEE=1", mc).await?;

        let ident =
            |lines: Option<Vec<String>>, cmd: &str| lines.and_then(|l| parser::info_line(&l, cmd));
        let manufacturer = ident(self.opt(at, "AT+CGMI", mc).await?, "AT+CGMI");
        let model = ident(self.opt(at, "AT+CGMM", mc).await?, "AT+CGMM");
        let firmware = ident(self.opt(at, "AT+CGMR", mc).await?, "AT+CGMR")
            .map(|f| f.trim_start_matches("Revision:").to_string());
        let imei = ident(self.opt(at, "AT+CGSN", mc).await?, "AT+CGSN")
            .filter(|s| s.chars().all(|c| c.is_ascii_digit()));
        let profile = self.deps.profiles.resolve(
            &mc.profile,
            manufacturer.as_deref().unwrap_or(""),
            model.as_deref().unwrap_or(""),
        );
        tracing::info!(modem = %self.id, manufacturer = ?manufacturer, model = ?model, firmware = ?firmware, profile = %profile.name, "modem identified");
        self.update(|s| {
            s.manufacturer = manufacturer;
            s.model = model;
            s.firmware = firmware;
            s.imei = imei;
            s.profile = Some(profile.name.clone());
        });

        let caps = self.detect_capabilities(mc, at, &profile).await?;
        self.update(|s| s.capabilities = caps);

        for cmd in profile.init_commands.iter().chain(&mc.init_commands) {
            if self.opt(at, cmd, mc).await?.is_none() {
                tracing::warn!(modem = %self.id, command = %cmd, "initialisation command was rejected");
            }
        }
        if mc.preferred_network != "auto" || profile.network_modes.contains_key("auto") {
            match profile.network_modes.get(&mc.preferred_network) {
                Some(cmd) => {
                    self.opt(at, cmd, mc).await?;
                }
                None if mc.preferred_network != "auto" => {
                    tracing::warn!(modem = %self.id, preferred = %mc.preferred_network, profile = %profile.name, "profile cannot select the preferred network type; setting ignored")
                }
                None => {}
            }
        }

        // SIM identity is readable before PIN entry on most modems.
        let mut iccid = None;
        for cmd in &profile.iccid_commands {
            if let Some(v) = ident(self.opt(at, cmd, mc).await?, cmd) {
                let digits: String = v.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
                if digits.len() >= 18 {
                    iccid = Some(digits.trim_end_matches(['f', 'F']).to_string());
                    break;
                }
            }
        }
        let cfg = self.deps.cfg.load_full();
        let sim = iccid
            .as_deref()
            .and_then(|i| {
                cfg.sims
                    .iter()
                    .find(|s| !s.iccid.is_empty() && s.iccid == i)
            })
            .or_else(|| cfg.sim_for_modem(&self.id))
            .cloned();
        self.update(|s| {
            s.sim.iccid = iccid.clone();
            s.sim.profile = sim.as_ref().map(|p| p.id.clone());
        });

        let sim_ready = self
            .unlock_sim(mc, at, sim.as_ref(), iccid.as_deref())
            .await?;
        if sim_ready {
            self.configure(mc, at, &profile, sim.as_ref()).await?;
        }
        Ok(Init {
            profile,
            sim,
            sim_ready,
        })
    }

    async fn detect_capabilities(
        &self,
        mc: &ModemConfig,
        at: &AtHandle,
        profile: &ModemProfile,
    ) -> Result<Capabilities, End> {
        let host = host_capture_devices();
        let mut caps = Capabilities {
            detected: false,
            sms_modes: vec![],
            network_technologies: profile.network_technologies.clone(),
            voice: profile.voice.clone(),
            delivery_report_routing: "unknown".into(),
            caller_id: "unknown".into(),
            ussd: "unknown".into(),
            gnss: if profile.gnss.kind.is_empty() {
                "unknown".into()
            } else {
                "unverified".into()
            },
            mms: "unknown".into(),
            bluetooth: "unknown".into(),
            audio: AudioCapabilities {
                modem_interfaces: profile.audio_interfaces.clone(),
                // Call audio reaches the host only through a dedicated audio
                // path (sound card wired to the modem, USB audio, PCM); it is
                // never available on the AT serial line.
                call_recording: if host.is_empty() {
                    "unsupported".into()
                } else {
                    "unverified".into()
                },
                host_capture_devices: host,
            },
            ..Default::default()
        };
        if !mc.auto_detect_capabilities {
            return Ok(caps);
        }
        caps.detected = true;
        let groups = |lines: Option<Vec<String>>, prefix: &str| -> Option<Vec<Vec<String>>> {
            lines?
                .iter()
                .find_map(|l| l.strip_prefix(prefix))
                .map(parser::parse_groups)
        };
        if let Some(g) = groups(self.opt(at, "AT+CMGF=?", mc).await?, "+CMGF:") {
            let modes = g.first().cloned().unwrap_or_default();
            caps.sms_modes = [("0", "pdu"), ("1", "text")]
                .iter()
                .filter(|(k, _)| modes.iter().any(|m| m == k))
                .map(|(_, v)| v.to_string())
                .collect();
        }
        if let Some(g) = groups(self.opt(at, "AT+CSCS=?", mc).await?, "+CSCS:") {
            caps.character_sets = g.first().cloned().unwrap_or_default();
        }
        if let Some(g) = groups(self.opt(at, "AT+IPR=?", mc).await?, "+IPR:") {
            let mut rates: Vec<u32> = g
                .iter()
                .flatten()
                .filter_map(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .collect();
            rates.sort_unstable();
            rates.dedup();
            caps.baud_rates = rates;
        }
        if let Some(g) = groups(self.opt(at, "AT+CPMS=?", mc).await?, "+CPMS:") {
            caps.sms_storages = g.first().cloned().unwrap_or_default();
        }
        if let Some(g) = groups(self.opt(at, "AT+CNMI=?", mc).await?, "+CNMI:") {
            caps.delivery_report_routing = support(
                g.get(3)
                    .map(|ds| ds.iter().any(|v| v == "1" || v == "2"))
                    .unwrap_or(false),
            );
        }
        caps.caller_id = support(self.opt(at, "AT+CLIP=?", mc).await?.is_some());
        caps.ussd = support(self.opt(at, "AT+CUSD=?", mc).await?.is_some());
        if !profile.gnss.test_command.is_empty() {
            caps.gnss = support(
                self.opt(at, &profile.gnss.test_command, mc)
                    .await?
                    .is_some(),
            );
        }
        if !profile.mms_test_command.is_empty() {
            // The modem exposes an MMS command set; retrieval through it is not
            // implemented by the gateway, hence "unverified" rather than supported.
            caps.mms = if self.opt(at, &profile.mms_test_command, mc).await?.is_some() {
                "unverified".into()
            } else {
                "unsupported".into()
            };
        }
        if !profile.bluetooth_test_command.is_empty() {
            caps.bluetooth = support(
                self.opt(at, &profile.bluetooth_test_command, mc)
                    .await?
                    .is_some(),
            );
        }
        if self.opt(at, "AT+CEREG?", mc).await?.is_some()
            && !caps.network_technologies.iter().any(|t| t == "LTE")
        {
            caps.network_technologies.push("LTE".into());
        }
        if caps.voice == "unknown" && caps.caller_id == "supported" {
            caps.voice = "unverified".into();
        }
        Ok(caps)
    }

    /// Bring the SIM to the READY state if that is possible without risk.
    /// A wrong PIN is never retried and a PUK is never attempted.
    pub(super) async fn unlock_sim(
        &mut self,
        mc: &ModemConfig,
        at: &AtHandle,
        sim: Option<&SimProfile>,
        iccid: Option<&str>,
    ) -> Result<bool, End> {
        let state = match self.q(at, "AT+CPIN?", mc).await {
            Ok(lines) => lines
                .iter()
                .find_map(|l| l.strip_prefix("+CPIN:"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            Err(AtError::Closed) => return Err(lost("serial connection closed")),
            Err(AtError::Rejected(e)) => {
                let missing = e.contains(": 10") || e.to_lowercase().contains("not inserted");
                self.sim_problem(if missing { "missing" } else { "error" }, &format!("SIM not usable ({e})"), "Insert a SIM card (power the module off first) and check that it is seated correctly.").await;
                return Ok(false);
            }
            Err(e) => {
                self.sim_problem(
                    "error",
                    &format!("SIM state query failed: {e}"),
                    "The modem did not report the SIM state; it is retried periodically.",
                )
                .await;
                return Ok(false);
            }
        };
        match state.as_str() {
            "READY" => {
                self.update(|s| s.sim.state = "ready".into());
                Ok(true)
            }
            "SIM PIN" => {
                let key = format!("pin_attempts:{}:{}", self.id, iccid.unwrap_or("-"));
                let used: u32 = self
                    .deps
                    .db
                    .kv_get(&key)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let Some(sim) = sim.filter(|s| s.pin_enabled && !s.pin.is_empty()) else {
                    self.sim_problem("pin_required", "SIM requires a PIN and none is configured", "Set pin and pin_enabled for this SIM in sim.conf, then reload or reconnect.").await;
                    return Ok(false);
                };
                if used >= sim.pin_max_attempts {
                    self.sim_problem(
                        "pin_required",
                        "SIM PIN was rejected earlier; not trying again",
                        "The configured PIN was not accepted. To protect the SIM from being blocked no further attempt is made. Correct the PIN in sim.conf and clear the counter with `thugsred-smsgateway modem reset-pin-counter`.",
                    )
                    .await;
                    return Ok(false);
                }
                // Persist the attempt before making it, so a crash cannot hide it.
                if self
                    .deps
                    .db
                    .kv_set(&key, &(used + 1).to_string())
                    .await
                    .is_err()
                {
                    self.sim_problem(
                        "pin_required",
                        "cannot record PIN attempt; not entering PIN",
                        "The database is unavailable; PIN entry is postponed.",
                    )
                    .await;
                    return Ok(false);
                }
                let cmd = AtCommand {
                    sensitive: true,
                    ..AtCommand::new(
                        format!("AT+CPIN=\"{}\"", sim.pin),
                        Duration::from_secs(mc.command_timeout_seconds),
                    )
                };
                match at.run(cmd).await {
                    Ok(_) => {
                        let _ = self.deps.db.kv_set(&key, "0").await;
                        for _ in 0..15 {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            if let Ok(l) = self.q(at, "AT+CPIN?", mc).await {
                                if l.iter().any(|x| x.contains("READY")) {
                                    self.update(|s| s.sim.state = "ready".into());
                                    tracing::info!(modem = %self.id, "SIM unlocked");
                                    return Ok(true);
                                }
                            }
                        }
                        self.sim_problem("error", "SIM did not become ready after PIN entry", "The PIN was accepted but the SIM is not ready yet; it is checked again periodically.").await;
                        Ok(false)
                    }
                    Err(AtError::Closed) => Err(lost("serial connection closed")),
                    Err(e) => {
                        self.sim_problem("pin_required", &format!("SIM PIN rejected ({e})"), "The configured PIN was not accepted. No further attempt is made until it is corrected and the counter is cleared.").await;
                        Ok(false)
                    }
                }
            }
            "SIM PUK" | "SIM PUK2" => {
                self.sim_problem("puk_required", "SIM is blocked and requires the PUK", "Unblock the SIM in a phone using the PUK from your operator. The gateway never enters a PUK.").await;
                Ok(false)
            }
            other => {
                self.sim_problem(
                    "error",
                    &format!("unexpected SIM state {other:?}"),
                    "The SIM reports a state the gateway cannot resolve automatically.",
                )
                .await;
                Ok(false)
            }
        }
    }

    async fn sim_problem(&mut self, sim_state: &str, error: &str, hint: &str) {
        tracing::warn!(modem = %self.id, sim_state, "{error}");
        let changed = self.status.borrow().sim.state != sim_state;
        self.update(|s| {
            s.sim.state = sim_state.to_string();
            s.last_error = Some(error.to_string());
            s.hint = Some(hint.to_string());
        });
        self.set_state(if sim_state == "error" || sim_state == "missing" {
            ModemState::Degraded
        } else {
            ModemState::SimLocked
        });
        if changed {
            self.emit("sim.locked", json!({"state": sim_state, "error": error}))
                .await;
        }
    }

    /// SMS and notification setup once the SIM is ready.
    pub(super) async fn configure(
        &mut self,
        mc: &ModemConfig,
        at: &AtHandle,
        profile: &ModemProfile,
        sim: Option<&SimProfile>,
    ) -> Result<(), End> {
        let cfg = self.deps.cfg.load_full();
        if self.opt(at, "AT+CMGF=0", mc).await?.is_none() {
            return Err(End::Failed {
                reason: "modem rejected PDU mode (AT+CMGF=0)".into(),
                hint: Some(
                    "This modem does not support SMS PDU mode, which the gateway requires.".into(),
                ),
                quick_retry: false,
            });
        }
        let storage = if !mc.sms_storage.is_empty() {
            mc.sms_storage.as_str()
        } else {
            profile.sms_storage.as_str()
        };
        if !storage.is_empty() {
            self.opt(
                at,
                &format!("AT+CPMS=\"{storage}\",\"{storage}\",\"{storage}\""),
                mc,
            )
            .await?;
        }
        if let Some(smsc) = sim.map(|s| s.smsc.as_str()).filter(|s| !s.is_empty()) {
            let toa = if smsc.starts_with('+') { 145 } else { 129 };
            self.opt(at, &format!("AT+CSCA=\"{smsc}\",{toa}"), mc)
                .await?;
        }
        let mut cnmi_ok = false;
        for mode in &profile.cnmi {
            if self
                .opt(at, &format!("AT+CNMI={mode}"), mc)
                .await?
                .is_some()
            {
                cnmi_ok = true;
                break;
            }
        }
        if !cnmi_ok {
            tracing::warn!(modem = %self.id, "no +CNMI setting accepted; relying on periodic polling for inbound messages");
        }
        self.opt(at, "AT+CLIP=1", mc).await?;
        self.opt(at, "AT+CREG=1", mc).await?;
        self.opt(at, "AT+CGREG=1", mc).await?;
        if cfg.ussd.enabled {
            self.opt(at, "AT+CUSD=1", mc).await?;
        }
        if let Some(sim) = sim {
            if sim.network.selection == "manual" && !sim.network.operator_code.is_empty() {
                self.opt(
                    at,
                    &format!("AT+COPS=1,2,\"{}\"", sim.network.operator_code),
                    mc,
                )
                .await?;
            }
        }
        // Subscriber identity.
        let ident =
            |lines: Option<Vec<String>>, cmd: &str| lines.and_then(|l| parser::info_line(&l, cmd));
        let imsi = ident(self.opt(at, "AT+CIMI", mc).await?, "AT+CIMI")
            .filter(|s| s.chars().all(|c| c.is_ascii_digit()));
        let msisdn = self
            .opt(at, "AT+CNUM", mc)
            .await?
            .and_then(|l| {
                l.iter().find_map(|x| {
                    x.strip_prefix("+CNUM:")
                        .map(|r| parser::split_fields(r.trim()))
                })
            })
            .and_then(|f| f.get(1).cloned())
            .filter(|n| !n.is_empty());
        let smsc = self
            .opt(at, "AT+CSCA?", mc)
            .await?
            .and_then(|l| {
                l.iter().find_map(|x| {
                    x.strip_prefix("+CSCA:")
                        .map(|r| parser::split_fields(r.trim()))
                })
            })
            .and_then(|f| f.first().cloned())
            .filter(|n| !n.is_empty());
        self.update(|s| {
            s.sim.imsi = imsi;
            s.sim.msisdn = msisdn;
            s.sim.smsc = smsc;
            s.sim.state = "ready".into();
        });
        let st = self.status.borrow().sim.clone();
        let _ = self
            .deps
            .db
            .upsert_sim_info(
                &self.id,
                st.profile.as_deref(),
                st.iccid.as_deref(),
                st.imsi.as_deref(),
                st.msisdn.as_deref(),
                None,
                Some("READY"),
                st.smsc.as_deref(),
            )
            .await;
        self.emit("sim.ready", json!({"sim_id": st.profile})).await;
        // GNSS receiver power, only when explicitly enabled for this modem.
        let gnss_here = cfg.gnss.enabled
            && (cfg.gnss.modem == self.id
                || (cfg.gnss.modem.is_empty()
                    && cfg.modems.first().map(|m| m.id == self.id).unwrap_or(false)));
        if gnss_here {
            match crate::gnss::commands(&profile.gnss.kind) {
                Some((power_on, _)) => {
                    self.opt(at, power_on, mc).await?;
                }
                None => {
                    tracing::warn!(modem = %self.id, profile = %profile.name, "GNSS is enabled but the modem profile has no GNSS support")
                }
            }
        }
        Ok(())
    }
}
