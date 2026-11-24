//! Opt-in automation: automatic replies and SMS forwarding.
//!
//! Loop prevention: automation only reacts to ordinary text SMS from full
//! telephone numbers (never alphanumeric senders or short codes), never to
//! the gateway's own numbers, and every rule is bounded by a per-sender
//! cooldown and an hourly cap.

use super::service::{SendRequest, SmsService};
use crate::config::Config;
use crate::database::{EventRecord, Message};
use crate::templates::{self, Escape};
use crate::{routing, utils};

fn is_person_number(n: &str) -> bool {
    let digits = n.trim_start_matches('+');
    digits.len() >= 7 && digits.chars().all(|c| c.is_ascii_digit())
}

impl SmsService {
    pub(crate) async fn run_automation(&self, cfg: &Config, m: &Message, ev: &EventRecord) {
        let Some(sender) = m
            .normalized_number
            .as_deref()
            .filter(|n| is_person_number(n))
        else {
            return;
        };
        // Never react to our own SIMs (another modem of this gateway or an echo).
        if cfg
            .sims
            .iter()
            .any(|s| !s.mobile_number.is_empty() && utils::numbers_match(&s.mobile_number, sender))
        {
            return;
        }
        for rule in cfg.automation.iter().filter(|r| r.enabled) {
            if !routing::filter_matches(&rule.filter, ev) {
                continue;
            }
            let hour_ago = utils::ts_after(-3600);
            let cooldown = utils::ts_after(-(rule.cooldown_seconds as i64));
            let (in_hour, by_sender) = match self
                .db
                .automation_counts(&rule.name, sender, &hour_ago, &cooldown)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(rule = %rule.name, error = %e, "automation bookkeeping failed; rule skipped");
                    continue;
                }
            };
            if in_hour >= rule.max_per_hour as i64 || (rule.cooldown_seconds > 0 && by_sender > 0) {
                tracing::info!(rule = %rule.name, "automation rule suppressed by rate limit or cooldown");
                continue;
            }
            let text = templates::render(
                &rule.template,
                &crate::integrations::context(ev, cfg),
                Escape::None,
            );
            if text.trim().is_empty() {
                continue;
            }
            let targets: Vec<String> = if rule.action == "forward" {
                rule.to
                    .iter()
                    .filter(|t| !utils::numbers_match(t, sender))
                    .cloned()
                    .collect()
            } else {
                vec![sender.to_string()]
            };
            if targets.is_empty() {
                continue;
            }
            // Record before sending so a failure cannot cause a tight loop.
            if self
                .db
                .log_automation(&rule.name, sender, &m.id)
                .await
                .is_err()
            {
                continue;
            }
            for to in targets {
                let req = SendRequest {
                    to,
                    message: Some(text.clone()),
                    delivery_report: Some(false),
                    priority: -10,
                    modem: Some(if rule.modem.is_empty() {
                        m.modem_id.clone().unwrap_or_default()
                    } else {
                        rule.modem.clone()
                    })
                    .filter(|s| !s.is_empty()),
                    correlation_id: Some(m.id.clone()),
                    origin: format!("automation:{}", rule.name),
                    ..Default::default()
                };
                match self.submit(req).await {
                    Ok(s) => {
                        tracing::info!(target: "audit", rule = %rule.name, action = %rule.action, message_id = %s.message.id, trigger = %m.id, "automation queued a message")
                    }
                    Err(e) => {
                        tracing::warn!(rule = %rule.name, error = %e, "automation could not queue a message")
                    }
                }
            }
        }
    }
}
