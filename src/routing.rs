//! Deterministic event routing and filtering.
//!
//! Precedence (documented in docs/INTEGRATIONS.md):
//! 1. Routing rules are evaluated in file order. A rule matches when the
//!    event type is listed and its filter passes.
//! 2. Matching rules contribute their destinations; `stop_processing` ends
//!    evaluation after that rule.
//! 3. If no rule matched, every enabled integration is a candidate.
//! 4. A candidate integration only receives the event if it is enabled,
//!    subscribes to the event type and its own filter passes.

use crate::config::{event_matches, Config, Filter};
use crate::database::EventRecord;
use crate::utils::list_matches;
use regex::Regex;
use serde_json::Value as Json;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn cached_regex(pattern: &str) -> Option<Regex> {
    static CACHE: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(r) = cache.get(pattern) {
        return Some(r.clone());
    }
    let re = regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .ok()?;
    if cache.len() >= 256 {
        cache.clear();
    }
    cache.insert(pattern.to_string(), re.clone());
    Some(re)
}

fn str_at<'a>(data: &'a Json, paths: &[&str]) -> Option<&'a str> {
    paths
        .iter()
        .find_map(|p| crate::templates::lookup(data, p).and_then(Json::as_str))
}

fn int_at(data: &Json, path: &str) -> Option<i64> {
    crate::templates::lookup(data, path).and_then(Json::as_i64)
}

fn allow(list: &[String], value: Option<&str>) -> bool {
    list.is_empty()
        || value
            .map(|v| list.iter().any(|p| list_matches(p, v)))
            .unwrap_or(false)
}

fn block(list: &[String], value: Option<&str>) -> bool {
    value
        .map(|v| list.iter().any(|p| list_matches(p, v)))
        .unwrap_or(false)
}

fn one_of(list: &[String], value: Option<&str>) -> bool {
    list.is_empty()
        || value
            .map(|v| list.iter().any(|p| p.eq_ignore_ascii_case(v)))
            .unwrap_or(false)
}

/// Evaluate a filter against an event.
pub fn filter_matches(f: &Filter, ev: &EventRecord) -> bool {
    let d = &ev.data;
    let sender = str_at(d, &["message.sender", "call.number"]);
    let recipient = str_at(d, &["message.recipient"]);
    let body = str_at(d, &["message.body"]);
    if !allow(&f.sender_allowlist, sender) || block(&f.sender_blocklist, sender) {
        return false;
    }
    if !allow(&f.recipient_allowlist, recipient) || block(&f.recipient_blocklist, recipient) {
        return false;
    }
    if !f.sender_regex.is_empty() {
        match (cached_regex(&f.sender_regex), sender) {
            (Some(re), Some(s)) if re.is_match(s) => {}
            _ => return false,
        }
    }
    if !f.message_regex.is_empty() {
        match (cached_regex(&f.message_regex), body) {
            (Some(re), Some(b)) if re.is_match(b) => {}
            _ => return false,
        }
    }
    if !f.message_contains.is_empty() {
        let Some(b) = body.map(str::to_lowercase) else {
            return false;
        };
        if !f
            .message_contains
            .iter()
            .any(|n| b.contains(&n.to_lowercase()))
        {
            return false;
        }
    }
    if !one_of(
        &f.directions,
        str_at(d, &["message.direction", "call.direction"]),
    ) || !one_of(&f.message_types, str_at(d, &["message.type"]))
        || !one_of(&f.encodings, str_at(d, &["message.encoding"]))
        || !one_of(&f.instance_ids, Some(&ev.instance_id))
        || !one_of(
            &f.modem_ids,
            str_at(d, &["message.modem_id", "modem_id", "call.modem_id"]),
        )
        || !one_of(&f.sim_ids, str_at(d, &["message.sim_id", "sim_id"]))
        || !one_of(
            &f.delivery_statuses,
            str_at(d, &["message.delivery_status", "message.status"]),
        )
    {
        return false;
    }
    if !f.message_classes.is_empty() {
        match int_at(d, "message.class") {
            Some(c) if f.message_classes.contains(&c) => {}
            _ => return false,
        }
    }
    if !f.application_ports.is_empty() {
        let ports = [int_at(d, "message.dst_port"), int_at(d, "message.src_port")];
        if !ports
            .iter()
            .flatten()
            .any(|p| f.application_ports.contains(p))
        {
            return false;
        }
    }
    true
}

/// Integrations that must receive this event, in configuration order.
pub fn destinations(cfg: &Config, ev: &EventRecord) -> Vec<String> {
    if !cfg.integrations.enabled {
        return Vec::new();
    }
    let mut matched_rule = false;
    let mut candidates: Vec<&str> = Vec::new();
    for rule in cfg.routing.iter().filter(|r| r.enabled) {
        if !rule.events.iter().any(|p| event_matches(p, &ev.event_type))
            || !filter_matches(&rule.filter, ev)
        {
            continue;
        }
        matched_rule = true;
        for d in &rule.destinations {
            if !candidates.contains(&d.as_str()) {
                candidates.push(d);
            }
        }
        if rule.stop_processing {
            break;
        }
    }
    cfg.integration
        .iter()
        .filter(|i| i.enabled)
        .filter(|i| !matched_rule || candidates.contains(&i.name.as_str()))
        .filter(|i| i.events.iter().any(|p| event_matches(p, &ev.event_type)))
        .filter(|i| filter_matches(&i.filter, ev))
        .map(|i| i.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{IntegrationDef, RoutingRule};
    use serde_json::json;

    fn ev(kind: &str, sender: &str, body: &str) -> EventRecord {
        EventRecord {
            id: "e".into(),
            event_type: kind.into(),
            instance_id: "gw".into(),
            correlation_id: None,
            timestamp: "t".into(),
            data: json!({"message": {"sender": sender, "body": body, "direction": "inbound", "type": "sms", "class": 1, "dst_port": 2948, "encoding": "gsm7"}}),
        }
    }

    fn integ(name: &str, events: &[&str]) -> IntegrationDef {
        IntegrationDef {
            name: name.into(),
            enabled: true,
            events: events.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn cfg() -> Config {
        let mut c = Config::default();
        c.integrations.enabled = true;
        c.integration = vec![
            integ("discord", &["sms.received"]),
            integ("teams", &["sms.*"]),
            integ("hook", &["*"]),
        ];
        c
    }

    #[test]
    fn default_routing_uses_subscriptions() {
        let c = cfg();
        assert_eq!(
            destinations(&c, &ev("sms.received", "+4500000000", "x")),
            ["discord", "teams", "hook"]
        );
        assert_eq!(destinations(&c, &ev("modem.connected", "", "")), ["hook"]);
        let mut off = cfg();
        off.integrations.enabled = false;
        assert!(destinations(&off, &ev("sms.received", "a", "b")).is_empty());
        let mut dis = cfg();
        dis.integration[0].enabled = false;
        assert_eq!(
            destinations(&dis, &ev("sms.received", "a", "b")),
            ["teams", "hook"]
        );
    }

    #[test]
    fn rules_restrict_and_stop() {
        let mut c = cfg();
        c.routing = vec![
            RoutingRule {
                name: "private".into(),
                events: vec!["sms.received".into()],
                filter: Filter {
                    sender_allowlist: vec!["+4500000000".into()],
                    ..Default::default()
                },
                stop_processing: true,
                ..Default::default()
            },
            RoutingRule {
                name: "alerts".into(),
                events: vec!["sms.received".into()],
                destinations: vec!["discord".into(), "teams".into()],
                filter: Filter {
                    sender_allowlist: vec!["ExampleService".into()],
                    message_regex: "(?i)(alert|critical)".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        // private sender: matched rule with no destinations -> nothing forwarded
        assert!(destinations(&c, &ev("sms.received", "+45 00 00 00 00", "CRITICAL")).is_empty());
        // alert from the service -> only the rule destinations
        assert_eq!(
            destinations(&c, &ev("sms.received", "exampleservice", "Critical: disk")),
            ["discord", "teams"]
        );
        // no rule matches -> default subscriptions
        assert_eq!(
            destinations(&c, &ev("sms.received", "ExampleService", "hello")).len(),
            3
        );
        // rule destinations still respect the integration's own subscription
        c.integration[0].events = vec!["sms.sent".into()];
        assert_eq!(
            destinations(&c, &ev("sms.received", "ExampleService", "alert")),
            ["teams"]
        );
    }

    #[test]
    fn filter_fields() {
        let e = ev("sms.received", "+4512345678", "Your code is 1234");
        let m = |f: Filter| filter_matches(&f, &e);
        assert!(m(Filter::default()));
        assert!(m(Filter {
            sender_allowlist: vec!["+45*".into()],
            ..Default::default()
        }));
        assert!(!m(Filter {
            sender_blocklist: vec!["+4512345678".into()],
            ..Default::default()
        }));
        assert!(m(Filter {
            message_contains: vec!["CODE".into()],
            ..Default::default()
        }));
        assert!(!m(Filter {
            message_contains: vec!["nope".into()],
            ..Default::default()
        }));
        assert!(m(Filter {
            sender_regex: r"^\+45\d{8}$".into(),
            ..Default::default()
        }));
        assert!(m(Filter {
            directions: vec!["inbound".into()],
            message_types: vec!["SMS".into()],
            ..Default::default()
        }));
        assert!(!m(Filter {
            directions: vec!["outbound".into()],
            ..Default::default()
        }));
        assert!(m(Filter {
            message_classes: vec![1],
            application_ports: vec![2948],
            ..Default::default()
        }));
        assert!(!m(Filter {
            message_classes: vec![0],
            ..Default::default()
        }));
        assert!(m(Filter {
            instance_ids: vec!["gw".into()],
            encodings: vec!["gsm7".into()],
            ..Default::default()
        }));
        assert!(!m(Filter {
            modem_ids: vec!["modem-9".into()],
            ..Default::default()
        }));
        // allowlist with an event that has no sender never matches
        let no_sender = EventRecord {
            data: json!({}),
            ..e.clone()
        };
        assert!(!filter_matches(
            &Filter {
                sender_allowlist: vec!["x".into()],
                ..Default::default()
            },
            &no_sender
        ));
        assert!(filter_matches(
            &Filter {
                sender_blocklist: vec!["x".into()],
                ..Default::default()
            },
            &no_sender
        ));
    }
}
