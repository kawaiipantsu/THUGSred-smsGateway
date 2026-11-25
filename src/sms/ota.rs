//! Detection and decoding of SIM Toolkit over-the-air (OTA) messages that
//! arrive as binary SMS, including S@T Browser (SIMalliance Toolbox Browser)
//! and WIB pushes of the kind used by the Simjacker and WIBattack techniques.
//!
//! This module is entirely **read-only**: it inspects an inbound
//! SMS-PP "SIM data download" message and reports what it is and what it was
//! trying to reach (the OTA command packet, its security level and its Toolkit
//! Application Reference). It never builds such a message. On a honeypot number
//! it turns a silent attack packet into a described, alertable event.
//!
//! References: 3GPP TS 23.040 (SMS), TS 31.115 / ETSI TS 102 225 (secured
//! packet structure), TS 31.111 / ETSI TS 102 223 (SIM Toolkit).

use super::pdu::decode::Deliver;
use super::pdu::types::Alphabet;
use serde::Serialize;

/// The decoded command security level of an OTA packet (from the SPI).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Security {
    /// none, redundancy_check, cryptographic_checksum or digital_signature.
    pub integrity: &'static str,
    pub ciphered: bool,
    /// True when there is neither integrity protection nor ciphering. An
    /// unsecured push to a SIM is the hallmark of a Simjacker/WIBattack
    /// attempt: the attack only works because the browser accepts it.
    pub unsecured: bool,
}

/// Result of inspecting an inbound SIM data-download message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OtaAnalysis {
    /// Why this was recognised as SIM Toolkit OTA (class 2, PID 0x7F, …).
    pub indicator: &'static str,
    /// True when a secured command packet (TS 102 225) was parsed.
    pub command_packet: bool,
    /// Toolkit Application Reference, three octets, as hex.
    pub tar: Option<String>,
    /// Human name for the TAR (RFM, RAM, S@T Browser, …).
    pub tar_name: &'static str,
    pub security: Option<Security>,
    /// Likely application behind the TAR: file_management, applet_management,
    /// sat_browser, wib_browser or unknown.
    pub application: &'static str,
    /// True when the message is consistent with a Simjacker/WIBattack-class
    /// attack (an unsecured or browser-targeted SIM push).
    pub suspected_attack: bool,
    /// One-line human summary for logs and alerts.
    pub summary: String,
}

impl OtaAnalysis {
    pub const MESSAGE_TYPE: &'static str = "sim_toolkit";
}

/// Interpret the SPI (two security-parameter octets) of a command packet.
fn security(spi1: u8) -> Security {
    let integrity = match spi1 & 0b11 {
        0 => "none",
        1 => "redundancy_check",
        2 => "cryptographic_checksum",
        _ => "digital_signature",
    };
    let ciphered = spi1 & 0b100 != 0;
    Security {
        integrity,
        ciphered,
        unsecured: integrity == "none" && !ciphered,
    }
}

/// Name and application for a Toolkit Application Reference.
///
/// The browser TARs are those reported for S@T Browser and WIB pushes in the
/// public Simjacker/WIBattack research; an operator may use others, so the raw
/// TAR is always reported alongside this label.
fn tar_meaning(tar: &[u8; 3]) -> (&'static str, &'static str) {
    match tar {
        [0x00, 0x00, 0x00] => ("Remote File Management (RFM)", "file_management"),
        [0x00, 0x00, 0x01] => ("Remote Applet Management (RAM)", "applet_management"),
        // S@T Browser pushes reported in Simjacker research use a TAR in the
        // 0xB0 00 xx range.
        [0xB0, 0x00, _] => ("S@T Browser (SIMalliance Toolbox)", "sat_browser"),
        // WIB (Wireless Internet Browser) pushes, as used by WIBattack.
        [0x53, 0x30, 0x60] | [0xBF, 0xFF, 0xFF] => {
            ("WIB (Wireless Internet Browser)", "wib_browser")
        }
        _ => ("application-specific", "unknown"),
    }
}

/// Inspect a delivered message. `payload` is the TP-User-Data without any user
/// data header (raw octets for a binary message). Returns `None` when the
/// message is not SIM-directed data download.
pub fn analyze(d: &Deliver, payload: &[u8]) -> Option<OtaAnalysis> {
    let dcs = &d.data_coding_scheme;
    // SMS-PP download to the SIM is marked by message class 2 and/or the
    // "(U)SIM data download" protocol identifier (0x7F), carried as 8-bit data.
    let class2 = dcs.message_class == Some(2);
    let sim_pid = d.protocol_identifier == 0x7F;
    let binary = dcs.alphabet == Alphabet::Binary || d.user_data.encoding == Alphabet::Binary;
    if !(binary && (class2 || sim_pid)) {
        return None;
    }
    let indicator = match (class2, sim_pid) {
        (true, true) => "class 2 (SIM) + PID 0x7F (SIM data download)",
        (true, false) => "message class 2 (SIM-specific)",
        _ => "PID 0x7F (SIM data download)",
    };

    // Try to parse a secured command packet (TS 102 225):
    //   CPL(2) CHL(1) SPI(2) KIc(1) KID(1) TAR(3) ...
    let mut command_packet = false;
    let mut tar = None;
    let mut tar_name = "unknown";
    let mut application = "unknown";
    let mut sec = None;
    if payload.len() >= 10 {
        let cpl = u16::from_be_bytes([payload[0], payload[1]]) as usize;
        // CPL counts every octet after itself; allow slack for padding.
        if cpl >= 11 && cpl <= payload.len() {
            let t = [payload[7], payload[8], payload[9]];
            let (name, app) = tar_meaning(&t);
            command_packet = true;
            tar = Some(hex(&t));
            tar_name = name;
            application = app;
            sec = Some(security(payload[3]));
        }
    }

    let browser = matches!(application, "sat_browser" | "wib_browser");
    let unsecured = sec.as_ref().map(|s| s.unsecured).unwrap_or(false);
    // A browser-targeted push, or any SIM push with no security, is treated as
    // an attack attempt. On a honeypot number a legitimate OTA is not expected.
    let suspected_attack = browser || unsecured;

    let mut summary = format!("SIM Toolkit OTA (SMS-PP data download): {indicator}");
    if let Some(t) = &tar {
        summary.push_str(&format!("; TAR {t} ({tar_name})"));
    }
    if let Some(s) = &sec {
        summary.push_str(&format!(
            "; integrity {}, {}",
            s.integrity,
            if s.ciphered {
                "ciphered"
            } else {
                "not ciphered"
            }
        ));
    }
    if browser {
        summary.push_str("; consistent with an S@T/WIB browser push (Simjacker/WIBattack class)");
    } else if unsecured {
        summary.push_str("; unsecured push to the SIM (attack hallmark)");
    }

    Some(OtaAnalysis {
        indicator,
        command_packet,
        tar,
        tar_name,
        security: sec,
        application,
        suspected_attack,
        summary,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sms::pdu::{decode, Pdu, PduDirection};

    /// Build an SMS-DELIVER PDU carrying a binary SIM data-download payload.
    fn deliver(pid: u8, dcs: u8, ud: &[u8]) -> Deliver {
        let mut p = String::from("00"); // no SMSC
        p.push_str("04"); // first octet: SMS-DELIVER
        p.push_str("0C915200000000F0"); // originating address (placeholder)
        p.push_str(&format!("{pid:02X}{dcs:02X}"));
        p.push_str("00000000000000"); // timestamp (7 octets)
        p.push_str(&format!("{:02X}", ud.len()));
        for b in ud {
            p.push_str(&format!("{b:02X}"));
        }
        match decode(&p, PduDirection::MobileTerminated, true).unwrap() {
            Pdu::Deliver(d) => *d,
            _ => panic!("not a deliver"),
        }
    }

    /// Unsecured command packet to an S@T Browser TAR (Simjacker shape).
    fn simjacker_packet() -> Vec<u8> {
        // CPL CHL SPI(00 00 = no security) KIc KID TAR(B0 00 10) CNTR(5) PCNTR + data
        let mut v = vec![0x00, 0x11, 0x15, 0x00, 0x00, 0x00, 0x00, 0xB0, 0x00, 0x10];
        v.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // CNTR + PCNTR
        v.extend_from_slice(b"\x01\x02\x03"); // (opaque) secured data
        v
    }

    #[test]
    fn detects_simjacker_style_push() {
        let d = deliver(0x7F, 0xF6, &simjacker_packet()); // DCS 0xF6 = 8-bit, class 2
        let a = analyze(&d, &d.user_data.payload).expect("should detect");
        assert_eq!(a.application, "sat_browser");
        assert_eq!(a.tar.as_deref(), Some("B00010"));
        assert!(a.command_packet);
        assert!(a.suspected_attack);
        assert!(a.security.as_ref().unwrap().unsecured);
        assert!(a.summary.contains("Simjacker"));
    }

    #[test]
    fn secured_rfm_is_ota_but_not_flagged() {
        // CC integrity (SPI1 = 0x02), RFM TAR 00 00 00.
        let mut ud = vec![0x00, 0x11, 0x15, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        ud.extend_from_slice(&[0; 9]);
        let d = deliver(0x7F, 0xF6, &ud);
        let a = analyze(&d, &d.user_data.payload).expect("should detect");
        assert_eq!(a.application, "file_management");
        assert_eq!(
            a.security.as_ref().unwrap().integrity,
            "cryptographic_checksum"
        );
        assert!(!a.suspected_attack);
    }

    #[test]
    fn class2_without_a_parsable_packet_is_still_flagged_structurally() {
        let d = deliver(0x00, 0xF6, &[0x01, 0x02, 0x03]); // class 2, too short for a packet
        let a = analyze(&d, &d.user_data.payload).expect("class 2 is SIM-directed");
        assert!(!a.command_packet);
        assert_eq!(a.tar, None);
        // No security could be read, so it is not auto-flagged as unsecured.
        assert!(!a.suspected_attack);
        assert_eq!(a.indicator, "message class 2 (SIM-specific)");
    }

    #[test]
    fn ordinary_binary_sms_is_not_ota() {
        let d = deliver(0x00, 0x04, &[0xDE, 0xAD, 0xBE, 0xEF]); // 8-bit, no class
        assert!(analyze(&d, &d.user_data.payload).is_none());
    }

    #[test]
    fn plain_text_sms_is_not_ota() {
        let d = deliver(0x00, 0x00, &[0xC8, 0x32, 0x9B, 0xFD, 0x06]); // GSM7 "hello"
        assert!(analyze(&d, &d.user_data.payload).is_none());
    }
}
