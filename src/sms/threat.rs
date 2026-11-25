//! Classification of inbound SMS-layer attacks for honeypot numbers.
//!
//! This is defensive and read-only: it labels a received message with an
//! attack category and severity so detections can be logged, alerted on and
//! counted per number. It does not generate any attack.
//!
//! Only SMS-layer techniques visible to a receiving modem are covered.
//! Network-layer (SS7/Diameter) attacks such as location tracking or SMS
//! interception are not visible to a subscriber modem and are out of scope.

use super::ota::OtaAnalysis;
use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// Attack categories that can be told apart from a received message.
pub const CATEGORIES: &[&str] = &[
    "simjacker",
    "wibattack",
    "sim_ota",
    "silent_sms",
    "oma_cp",
    "smishing",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Threat {
    pub category: &'static str,
    /// high, medium or info.
    pub severity: &'static str,
    /// Human name of the technique.
    pub name: &'static str,
    /// Heuristic confidence of malicious intent, 0-100 (a weighted score, not
    /// a calibrated probability). Honeypot numbers receive no legitimate
    /// traffic, so even a moderate score is worth review.
    pub confidence: u8,
    pub detail: String,
}

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:https?://|www\.)[^\s]+").unwrap());

const SHORTENERS: &[&str] = &[
    "bit.ly",
    "tinyurl.com",
    "t.co",
    "goo.gl",
    "ow.ly",
    "is.gd",
    "buff.ly",
    "rebrand.ly",
    "cutt.ly",
    "shorturl.at",
    "rb.gy",
];
const LURE_WORDS: &[&str] = &[
    "verify", "login", "log in", "bank", "parcel", "package", "delivery", "suspend", "urgent",
    "password", "otp", "one-time", "code", "account", "confirm", "unlock", "gift", "prize", "won",
    "refund", "tax", "fine", "nemid", "mitid", "bankid",
];

/// Score a smishing text (0-100) and describe the indicators found.
fn smishing_score(text: &str, link: &str) -> (u8, String) {
    let lower = text.to_lowercase();
    let host = link
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    let host = host.split(['/', '?', '#']).next().unwrap_or(host);
    let mut score: i32 = 40;
    let mut why: Vec<String> = Vec::new();
    if SHORTENERS.iter().any(|d| host.eq_ignore_ascii_case(d)) {
        score += 25;
        why.push("URL shortener".into());
    }
    if host.contains("xn--") {
        score += 25;
        why.push("punycode/IDN host (possible lookalike)".into());
    }
    if host.split('.').all(|p| p.parse::<u8>().is_ok()) && host.contains('.') {
        score += 20;
        why.push("IP-address host".into());
    }
    if URL_RE.find_iter(text).count() > 1 {
        score += 10;
        why.push("multiple links".into());
    }
    let hits: Vec<&str> = LURE_WORDS
        .iter()
        .filter(|w| lower.contains(*w))
        .copied()
        .collect();
    if !hits.is_empty() {
        score += (hits.len() as i32 * 8).min(24);
        why.push(format!("lure wording ({})", hits.join(", ")));
    }
    let score = score.clamp(40, 97) as u8;
    let mut detail = format!("Inbound text contains a link: {link}");
    if !why.is_empty() {
        detail.push_str(&format!("; indicators: {}", why.join("; ")));
    }
    (score, detail)
}

/// Classify a received message. `ota` is the SIM Toolkit analysis, when any.
pub fn classify(
    message_type: &str,
    protocol_id: u8,
    dst_port: Option<u16>,
    text: Option<&str>,
    ota: Option<&OtaAnalysis>,
) -> Option<Threat> {
    // SIM Toolkit OTA — the strongest and highest-value signal.
    if let Some(o) = ota {
        let (category, name) = match o.application {
            "sat_browser" => ("simjacker", "Simjacker (S@T Browser OTA)"),
            "wib_browser" => ("wibattack", "WIBattack (WIB OTA)"),
            _ => ("sim_ota", "SIM OTA command (SMS-PP data download)"),
        };
        let base = if category == "sim_ota" { 85 } else { 95 };
        return Some(Threat {
            category,
            severity: if o.suspected_attack { "high" } else { "medium" },
            name,
            confidence: if o.suspected_attack { base } else { base - 30 },
            detail: o.summary.clone(),
        });
    }
    // Silent SMS / Type 0 ping: acknowledged by the handset but not shown.
    if protocol_id == 0x40 {
        return Some(Threat {
            category: "silent_sms",
            severity: "high",
            name: "Silent SMS (Type 0 ping)",
            confidence: 85,
            detail: "TP-PID 0x40 (Short Message Type 0): acknowledged by the handset but not stored or shown; used to probe a subscriber's presence.".into(),
        });
    }
    // WAP provisioning push (OMA Client Provisioning) — spoofed settings.
    if message_type == "wap_push" && dst_port == Some(2948) {
        return Some(Threat {
            category: "oma_cp",
            severity: "medium",
            name: "WAP provisioning push (OMA CP)",
            confidence: 60,
            detail: "WAP Push to the provisioning port 2948; can carry spoofed device settings (OMA CP phishing).".into(),
        });
    }
    // Smishing: an inbound text carrying a link. Honeypot numbers should not
    // receive legitimate traffic, so a link is noteworthy.
    if matches!(message_type, "sms" | "flash") {
        if let Some(mat) = text.and_then(|t| URL_RE.find(t)) {
            let link: String = mat.as_str().chars().take(80).collect();
            let (confidence, detail) = smishing_score(text.unwrap_or(""), &link);
            return Some(Threat {
                category: "smishing",
                severity: if confidence >= 70 { "high" } else { "medium" },
                name: "Smishing (text with link)",
                confidence,
                detail,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_sms_by_pid() {
        let t = classify("service", 0x40, None, None, None).unwrap();
        assert_eq!((t.category, t.severity), ("silent_sms", "high"));
    }

    #[test]
    fn smishing_by_link() {
        assert!(classify("sms", 0, None, Some("hello there"), None).is_none());
        let t = classify(
            "sms",
            0,
            None,
            Some("Your parcel: https://dhl-track.example/x"),
            None,
        )
        .unwrap();
        assert_eq!(t.category, "smishing");
        assert!(t.detail.contains("https://"));
        // A shortener plus lure wording scores higher than a bare link.
        let bare = classify("sms", 0, None, Some("see https://example.test/a"), None).unwrap();
        let worse = classify(
            "sms",
            0,
            None,
            Some("Verify your bank account: https://bit.ly/x"),
            None,
        )
        .unwrap();
        assert!(worse.confidence > bare.confidence);
        let t = classify(
            "flash",
            0,
            None,
            Some("login at www.bank-verify.example now"),
            None,
        )
        .unwrap();
        assert_eq!(t.category, "smishing");
    }

    #[test]
    fn oma_cp_by_port() {
        let t = classify("wap_push", 0, Some(2948), None, None).unwrap();
        assert_eq!(t.category, "oma_cp");
        assert!(classify("wap_push", 0, Some(5000), None, None).is_none());
    }

    #[test]
    fn plain_sms_is_not_a_threat() {
        assert!(classify("sms", 0, None, Some("just a normal message"), None).is_none());
    }
}
