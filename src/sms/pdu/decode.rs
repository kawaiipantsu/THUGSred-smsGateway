//! Decoding of SMS-DELIVER, SMS-SUBMIT and SMS-STATUS-REPORT TPDUs.

use super::types::*;
use super::{parse_hex, PduError};
use serde::Serialize;

/// Which side produced the TPDU. The message type indicator is ambiguous
/// without it (MTI 1 is SMS-SUBMIT when mobile originated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PduDirection {
    /// Received from the network (SMS-DELIVER, SMS-STATUS-REPORT).
    MobileTerminated,
    /// Sent to the network (SMS-SUBMIT).
    MobileOriginated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Deliver {
    pub smsc: Option<Address>,
    pub first_octet: u8,
    pub more_messages_to_send: bool,
    pub status_report_indication: bool,
    pub reply_path: bool,
    pub user_data_header_indicator: bool,
    pub sender: Address,
    pub protocol_identifier: u8,
    pub protocol_description: &'static str,
    pub data_coding_scheme: Dcs,
    pub timestamp: Timestamp,
    pub user_data: UserData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Submit {
    pub smsc: Option<Address>,
    pub first_octet: u8,
    pub reject_duplicates: bool,
    pub status_report_request: bool,
    pub reply_path: bool,
    pub user_data_header_indicator: bool,
    pub message_reference: u8,
    pub recipient: Address,
    pub protocol_identifier: u8,
    pub protocol_description: &'static str,
    pub data_coding_scheme: Dcs,
    /// none, relative, enhanced or absolute
    pub validity_period_format: &'static str,
    pub validity_minutes: Option<u32>,
    pub validity_absolute: Option<Timestamp>,
    pub user_data: UserData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusReport {
    pub smsc: Option<Address>,
    pub first_octet: u8,
    pub message_reference: u8,
    pub recipient: Address,
    pub timestamp: Timestamp,
    pub discharge_time: Timestamp,
    pub status: u8,
    /// delivered, pending, temporary_failure or permanent_failure
    pub status_category: &'static str,
    pub status_description: &'static str,
    pub user_data: Option<UserData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Pdu {
    Deliver(Box<Deliver>),
    Submit(Box<Submit>),
    StatusReport(Box<StatusReport>),
}

/// Classify a TP-Status value (3GPP TS 23.040 section 9.2.3.15).
pub fn status_category(status: u8) -> (&'static str, &'static str) {
    match status {
        0x00 => ("delivered", "received by the recipient"),
        0x01 => ("delivered", "forwarded, delivery unconfirmed"),
        0x02 => ("delivered", "replaced by the service centre"),
        0x20 => ("pending", "congestion, still trying"),
        0x21 => ("pending", "recipient busy, still trying"),
        0x22 => ("pending", "no response from recipient, still trying"),
        0x23 => ("pending", "service rejected, still trying"),
        0x24 => ("pending", "quality of service unavailable, still trying"),
        0x25 => ("pending", "error in recipient, still trying"),
        0x40 => ("permanent_failure", "remote procedure error"),
        0x41 => ("permanent_failure", "incompatible destination"),
        0x42 => ("permanent_failure", "connection rejected by recipient"),
        0x43 => ("permanent_failure", "not obtainable"),
        0x44 => ("permanent_failure", "quality of service unavailable"),
        0x45 => ("permanent_failure", "no interworking available"),
        0x46 => ("permanent_failure", "validity period expired"),
        0x47 => ("permanent_failure", "deleted by originating entity"),
        0x48 => (
            "permanent_failure",
            "deleted by service centre administration",
        ),
        0x49 => ("permanent_failure", "message does not exist"),
        0x60 => ("temporary_failure", "congestion"),
        0x61 => ("temporary_failure", "recipient busy"),
        0x62 => ("temporary_failure", "no response from recipient"),
        0x63 => ("temporary_failure", "service rejected"),
        0x64 => ("temporary_failure", "quality of service unavailable"),
        0x65 => ("temporary_failure", "error in recipient"),
        0x03..=0x1F => (
            "delivered",
            "reserved or service centre specific completion",
        ),
        0x26..=0x3F => (
            "pending",
            "reserved or service centre specific, still trying",
        ),
        0x4A..=0x5F => ("permanent_failure", "reserved or service centre specific"),
        0x66..=0x7F => ("temporary_failure", "reserved or service centre specific"),
        _ => ("permanent_failure", "reserved"),
    }
}

/// Relative validity period octet to minutes.
pub fn relative_validity_minutes(v: u8) -> u32 {
    let v = v as u32;
    match v {
        0..=143 => (v + 1) * 5,
        144..=167 => 12 * 60 + (v - 143) * 30,
        168..=196 => (v - 166) * 24 * 60,
        _ => (v - 192) * 7 * 24 * 60,
    }
}

fn decode_deliver(smsc: Option<Address>, fo: u8, r: &mut Reader<'_>) -> Result<Deliver, PduError> {
    let sender = read_tp_address(r, "originating address")?;
    let pid = r.u8("protocol identifier")?;
    let dcs = Dcs::parse(r.u8("data coding scheme")?);
    let ts = Timestamp::parse(r.take(7, "service centre timestamp")?);
    let udl = r.u8("user data length")?;
    let udhi = fo & 0x40 != 0;
    let user_data = decode_user_data(udhi, &dcs, udl, r.rest())?;
    Ok(Deliver {
        smsc,
        first_octet: fo,
        more_messages_to_send: fo & 0x04 == 0,
        status_report_indication: fo & 0x20 != 0,
        reply_path: fo & 0x80 != 0,
        user_data_header_indicator: udhi,
        sender,
        protocol_identifier: pid,
        protocol_description: pid_description(pid),
        data_coding_scheme: dcs,
        timestamp: ts,
        user_data,
    })
}

fn decode_submit(smsc: Option<Address>, fo: u8, r: &mut Reader<'_>) -> Result<Submit, PduError> {
    let mr = r.u8("message reference")?;
    let recipient = read_tp_address(r, "destination address")?;
    let pid = r.u8("protocol identifier")?;
    let dcs = Dcs::parse(r.u8("data coding scheme")?);
    let (vpf, minutes, absolute) = match (fo >> 3) & 0x03 {
        0 => ("none", None, None),
        2 => (
            "relative",
            Some(relative_validity_minutes(r.u8("validity period")?)),
            None,
        ),
        1 => {
            r.take(7, "enhanced validity period")?;
            ("enhanced", None, None)
        }
        _ => (
            "absolute",
            None,
            Some(Timestamp::parse(r.take(7, "validity period")?)),
        ),
    };
    let udl = r.u8("user data length")?;
    let udhi = fo & 0x40 != 0;
    let user_data = decode_user_data(udhi, &dcs, udl, r.rest())?;
    Ok(Submit {
        smsc,
        first_octet: fo,
        reject_duplicates: fo & 0x04 != 0,
        status_report_request: fo & 0x20 != 0,
        reply_path: fo & 0x80 != 0,
        user_data_header_indicator: udhi,
        message_reference: mr,
        recipient,
        protocol_identifier: pid,
        protocol_description: pid_description(pid),
        data_coding_scheme: dcs,
        validity_period_format: vpf,
        validity_minutes: minutes,
        validity_absolute: absolute,
        user_data,
    })
}

fn decode_status_report(
    smsc: Option<Address>,
    fo: u8,
    r: &mut Reader<'_>,
) -> Result<StatusReport, PduError> {
    let mr = r.u8("message reference")?;
    let recipient = read_tp_address(r, "recipient address")?;
    let timestamp = Timestamp::parse(r.take(7, "service centre timestamp")?);
    let discharge_time = Timestamp::parse(r.take(7, "discharge time")?);
    let status = r.u8("status")?;
    // Optional TP-PI and the fields it announces.
    let mut user_data = None;
    if r.remaining() > 0 {
        let pi = r.u8("parameter indicator")?;
        // 0xFF is commonly used as padding by modems; treat as absent.
        if pi != 0xFF {
            if pi & 0x01 != 0 {
                r.u8("protocol identifier")?;
            }
            let dcs = if pi & 0x02 != 0 {
                Dcs::parse(r.u8("data coding scheme")?)
            } else {
                Dcs::parse(0)
            };
            if pi & 0x04 != 0 {
                let udl = r.u8("user data length")?;
                user_data = Some(decode_user_data(fo & 0x40 != 0, &dcs, udl, r.rest())?);
            }
        }
    }
    let (cat, desc) = status_category(status);
    Ok(StatusReport {
        smsc,
        first_octet: fo,
        message_reference: mr,
        recipient,
        timestamp,
        discharge_time,
        status,
        status_category: cat,
        status_description: desc,
        user_data,
    })
}

/// Decode a PDU from raw bytes.
///
/// `has_smsc` states whether the PDU starts with a service centre address,
/// which is the format modems use for `+CMT`, `+CMGR`, `+CMGL` and `+CMGS`.
pub fn decode_bytes(
    bytes: &[u8],
    direction: PduDirection,
    has_smsc: bool,
) -> Result<Pdu, PduError> {
    let mut r = Reader::new(bytes);
    let smsc = if has_smsc { read_smsc(&mut r)? } else { None };
    let fo = r.u8("first octet")?;
    match (fo & 0x03, direction) {
        (0, PduDirection::MobileTerminated) => {
            decode_deliver(smsc, fo, &mut r).map(|d| Pdu::Deliver(Box::new(d)))
        }
        (1, PduDirection::MobileOriginated) => {
            decode_submit(smsc, fo, &mut r).map(|d| Pdu::Submit(Box::new(d)))
        }
        (2, PduDirection::MobileTerminated) => {
            decode_status_report(smsc, fo, &mut r).map(|d| Pdu::StatusReport(Box::new(d)))
        }
        (0, PduDirection::MobileOriginated) => Err(PduError::Unsupported(
            "SMS-DELIVER-REPORT is not supported".into(),
        )),
        (1, PduDirection::MobileTerminated) => Err(PduError::Unsupported(
            "SMS-SUBMIT-REPORT is not supported".into(),
        )),
        (2, PduDirection::MobileOriginated) => {
            Err(PduError::Unsupported("SMS-COMMAND is not supported".into()))
        }
        _ => Err(PduError::Invalid("reserved message type indicator".into())),
    }
}

/// Decode a hexadecimal PDU.
pub fn decode(hex: &str, direction: PduDirection, has_smsc: bool) -> Result<Pdu, PduError> {
    decode_bytes(&parse_hex(hex)?, direction, has_smsc)
}

/// Offline inspection helper: try the plausible interpretations of a PDU and
/// return the first that parses cleanly. Preference order is modem format
/// (with SMSC) before bare TPDU, and mobile terminated before originated.
pub fn decode_auto(hex: &str) -> Result<Pdu, PduError> {
    let bytes = parse_hex(hex)?;
    let mut first_err = None;
    for has_smsc in [true, false] {
        for dir in [
            PduDirection::MobileTerminated,
            PduDirection::MobileOriginated,
        ] {
            match decode_bytes(&bytes, dir, has_smsc) {
                Ok(p) => {
                    let clean = match &p {
                        Pdu::Deliver(d) => d.user_data.warnings.is_empty(),
                        Pdu::Submit(s) => s.user_data.warnings.is_empty(),
                        Pdu::StatusReport(_) => true,
                    };
                    if clean {
                        return Ok(p);
                    }
                    first_err.get_or_insert(Ok(p));
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(Err(e));
                    }
                }
            }
        }
    }
    first_err.unwrap_or(Err(PduError::InvalidHex))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Widely published reference PDU: "hellohello" from +27838890001 via SMSC +27381000015.
    const HELLO: &str = "07917283010010F5040BC87238880900F10000993092516195800AE8329BFD4697D9EC37";

    #[test]
    fn decodes_reference_deliver() {
        let Pdu::Deliver(d) = decode(HELLO, PduDirection::MobileTerminated, true).unwrap() else {
            panic!("expected deliver")
        };
        assert_eq!(d.smsc.as_ref().unwrap().number, "+27381000015");
        assert_eq!(d.sender.number, "27838890001");
        assert_eq!(d.user_data.text.as_deref(), Some("hellohello"));
        assert_eq!(d.user_data.encoding, Alphabet::Gsm7);
        assert_eq!(d.timestamp.raw, "99/03/29,15:16:59+08");
        assert!(d.user_data.header.is_none());
    }

    #[test]
    fn truncated_pdus_never_panic() {
        let bytes = hex::decode(HELLO).unwrap();
        for len in 0..bytes.len() {
            for has_smsc in [true, false] {
                for dir in [
                    PduDirection::MobileTerminated,
                    PduDirection::MobileOriginated,
                ] {
                    let _ = decode_bytes(&bytes[..len], dir, has_smsc);
                }
            }
        }
        assert!(decode_bytes(
            &bytes[..bytes.len() - 1],
            PduDirection::MobileTerminated,
            true
        )
        .is_err());
    }

    #[test]
    fn garbage_never_panics() {
        // Deterministic pseudo-random fuzzing of the decoder.
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        for _ in 0..20_000 {
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            let len = (next() % 180) as usize;
            let buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            for has_smsc in [true, false] {
                for dir in [
                    PduDirection::MobileTerminated,
                    PduDirection::MobileOriginated,
                ] {
                    let _ = decode_bytes(&buf, dir, has_smsc);
                }
            }
        }
    }

    #[test]
    fn invalid_hex_rejected() {
        assert_eq!(
            decode("ZZ", PduDirection::MobileTerminated, true),
            Err(PduError::InvalidHex)
        );
        assert_eq!(
            decode("ABC", PduDirection::MobileTerminated, true),
            Err(PduError::InvalidHex)
        );
        assert_eq!(
            decode("", PduDirection::MobileTerminated, true),
            Err(PduError::InvalidHex)
        );
    }

    #[test]
    fn relative_validity() {
        assert_eq!(relative_validity_minutes(0), 5);
        assert_eq!(relative_validity_minutes(143), 720);
        assert_eq!(relative_validity_minutes(167), 1440);
        assert_eq!(relative_validity_minutes(168), 2 * 1440);
        assert_eq!(relative_validity_minutes(255), 63 * 7 * 1440);
    }

    #[test]
    fn status_categories() {
        assert_eq!(status_category(0x00).0, "delivered");
        assert_eq!(status_category(0x20).0, "pending");
        assert_eq!(status_category(0x46).0, "permanent_failure");
        assert_eq!(status_category(0x61).0, "temporary_failure");
    }
}
