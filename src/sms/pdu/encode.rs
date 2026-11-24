//! SMS-SUBMIT generation with automatic encoding selection and segmentation,
//! plus SMS-DELIVER / SMS-STATUS-REPORT builders used by the simulator and tests.

use super::types::*;
use super::{gsm7, PduError};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    #[default]
    Auto,
    Gsm7,
    Ucs2,
    #[serde(alias = "8bit")]
    Binary,
}

#[derive(Debug, Clone)]
pub enum Payload {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct SubmitRequest {
    pub to: String,
    /// Explicit service centre; `None` uses the SMSC stored on the SIM.
    pub smsc: Option<String>,
    pub payload: Payload,
    pub encoding: Encoding,
    /// When false, text that needs UCS-2 is rejected.
    pub allow_unicode: bool,
    pub message_class: Option<u8>,
    pub status_report: bool,
    pub validity_minutes: Option<u32>,
    pub protocol_identifier: u8,
    pub destination_port: Option<u16>,
    pub source_port: Option<u16>,
    /// Concatenation reference used when the message needs several parts.
    pub reference: u8,
    pub max_parts: usize,
    pub reject_duplicates: bool,
}

impl SubmitRequest {
    pub fn text(to: &str, text: &str) -> Self {
        Self {
            to: to.to_string(),
            smsc: None,
            payload: Payload::Text(text.to_string()),
            encoding: Encoding::Auto,
            allow_unicode: true,
            message_class: None,
            status_report: false,
            validity_minutes: None,
            protocol_identifier: 0,
            destination_port: None,
            source_port: None,
            reference: 0,
            max_parts: 10,
            reject_duplicates: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EncodedPart {
    pub sequence: u8,
    /// Full PDU in modem format (SMSC prefix + TPDU), uppercase hex.
    pub pdu: String,
    /// TPDU length in octets, the argument to `AT+CMGS=`.
    pub tpdu_length: usize,
    /// Text carried by this part (text messages only).
    pub text: Option<String>,
    /// Payload units of this part (septets, UCS-2 octets or binary octets).
    #[serde(with = "hexser")]
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EncodedMessage {
    pub encoding: Alphabet,
    pub data_coding_scheme: u8,
    pub reference: Option<u8>,
    pub parts: Vec<EncodedPart>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Estimate {
    pub encoding: Alphabet,
    pub parts: usize,
    pub characters: usize,
    /// Units used: septets for GSM 7-bit, UTF-16 code units for UCS-2, octets for binary.
    pub units: usize,
    pub units_per_part: usize,
    pub units_remaining: usize,
}

/// Minutes to the relative validity period octet, rounding up.
pub fn encode_relative_validity(minutes: u32) -> u8 {
    let m = minutes.max(5);
    if m <= 720 {
        (m.div_ceil(5) - 1) as u8
    } else if m <= 1440 {
        (143 + (m - 720).div_ceil(30)) as u8
    } else if m <= 30 * 1440 {
        (166 + m.div_ceil(1440)) as u8
    } else {
        (192 + m.div_ceil(7 * 1440)).min(255) as u8
    }
}

fn port_ie(dest: Option<u16>, src: Option<u16>) -> Vec<u8> {
    match (dest, src) {
        (None, None) => vec![],
        (d, s) => {
            let (d, s) = (d.unwrap_or(0), s.unwrap_or(0));
            let mut v = vec![0x05, 0x04];
            v.extend(d.to_be_bytes());
            v.extend(s.to_be_bytes());
            v
        }
    }
}

struct Units {
    alphabet: Alphabet,
    /// septets (one per byte), UCS-2 octets, or binary octets
    data: Vec<u8>,
    characters: usize,
}

fn select_units(
    payload: &Payload,
    encoding: Encoding,
    allow_unicode: bool,
) -> Result<Units, PduError> {
    match payload {
        Payload::Binary(b) => match encoding {
            Encoding::Auto | Encoding::Binary => Ok(Units {
                alphabet: Alphabet::Binary,
                data: b.clone(),
                characters: b.len(),
            }),
            _ => Err(PduError::Invalid(
                "binary payloads require 8-bit encoding".into(),
            )),
        },
        Payload::Text(t) => {
            let characters = t.chars().count();
            let ucs2 = || {
                if !allow_unicode {
                    return Err(PduError::Invalid(
                        "message requires Unicode (UCS-2) encoding, which is disabled".into(),
                    ));
                }
                Ok(Units {
                    alphabet: Alphabet::Ucs2,
                    data: t.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
                    characters,
                })
            };
            match encoding {
                Encoding::Binary => Err(PduError::Invalid(
                    "8-bit encoding requires a binary payload".into(),
                )),
                Encoding::Ucs2 => ucs2(),
                Encoding::Gsm7 => gsm7::encode_str(t)
                    .map(|data| Units {
                        alphabet: Alphabet::Gsm7,
                        data,
                        characters,
                    })
                    .ok_or_else(|| {
                        PduError::Invalid(
                            "message contains characters outside the GSM 7-bit alphabet".into(),
                        )
                    }),
                Encoding::Auto => match gsm7::encode_str(t) {
                    Some(data) => Ok(Units {
                        alphabet: Alphabet::Gsm7,
                        data,
                        characters,
                    }),
                    None => ucs2(),
                },
            }
        }
    }
}

/// Capacity in units of one part given the total user data header size
/// (including the UDHL octet; zero when there is no header).
fn capacity(alphabet: Alphabet, udh_total: usize) -> usize {
    match alphabet {
        Alphabet::Gsm7 => 160 - (udh_total * 8).div_ceil(7),
        Alphabet::Ucs2 => (140 - udh_total) / 2 * 2,
        Alphabet::Binary => 140 - udh_total,
    }
}

/// Split units into chunks of at most `cap`, never separating a GSM escape
/// sequence or a UTF-16 surrogate pair.
fn split_units(alphabet: Alphabet, data: &[u8], cap: usize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < data.len() {
        let mut end = (start + cap).min(data.len());
        if end < data.len() {
            match alphabet {
                Alphabet::Gsm7 => {
                    if data[end - 1] == gsm7::ESC && end - 1 > start {
                        end -= 1;
                    }
                }
                Alphabet::Ucs2 => {
                    let hi = data[end - 2];
                    if (0xD8..=0xDB).contains(&hi) && end - 2 > start {
                        end -= 2;
                    }
                }
                Alphabet::Binary => {}
            }
        }
        out.push(&data[start..end]);
        start = end;
    }
    if out.is_empty() {
        out.push(&data[0..0]);
    }
    out
}

fn layout(units: &Units, ports: &[u8]) -> (bool, usize) {
    let single_udh = if ports.is_empty() { 0 } else { ports.len() + 1 };
    if units.data.len() <= capacity(units.alphabet, single_udh) {
        (false, capacity(units.alphabet, single_udh))
    } else {
        (true, capacity(units.alphabet, ports.len() + 5 + 1))
    }
}

/// Build TP-UDL and TP-UD from a header (without UDHL) and payload units.
fn build_user_data(alphabet: Alphabet, udh: &[u8], units: &[u8]) -> (u8, Vec<u8>) {
    let mut ud = Vec::with_capacity(140);
    let header_len = if udh.is_empty() { 0 } else { udh.len() + 1 };
    if !udh.is_empty() {
        ud.push(udh.len() as u8);
        ud.extend_from_slice(udh);
    }
    match alphabet {
        Alphabet::Gsm7 => {
            let header_septets = (header_len * 8).div_ceil(7);
            let fill = header_septets * 7 - header_len * 8;
            let packed = gsm7::pack(units, fill);
            ud.extend_from_slice(&packed);
            ((header_septets + units.len()) as u8, ud)
        }
        _ => {
            ud.extend_from_slice(units);
            (ud.len() as u8, ud)
        }
    }
}

fn validate_common(req: &SubmitRequest) -> Result<(), PduError> {
    if let Some(c) = req.message_class {
        if c > 3 {
            return Err(PduError::Invalid("message class must be 0 to 3".into()));
        }
    }
    if req.max_parts == 0 || req.max_parts > 255 {
        return Err(PduError::Invalid("max_parts must be 1 to 255".into()));
    }
    Ok(())
}

/// Estimate encoding and segmentation without building PDUs.
pub fn estimate(
    payload: &Payload,
    encoding: Encoding,
    allow_unicode: bool,
    has_ports: bool,
) -> Result<Estimate, PduError> {
    let units = select_units(payload, encoding, allow_unicode)?;
    let ports = if has_ports { vec![0u8; 6] } else { vec![] };
    let (_, cap) = layout(&units, &ports);
    let parts = split_units(units.alphabet, &units.data, cap);
    let last = parts.last().map(|p| p.len()).unwrap_or(0);
    let div = if units.alphabet == Alphabet::Ucs2 {
        2
    } else {
        1
    };
    Ok(Estimate {
        encoding: units.alphabet,
        parts: parts.len(),
        characters: units.characters,
        units: units.data.len() / div,
        units_per_part: cap / div,
        units_remaining: (cap - last) / div,
    })
}

/// Encode a message into one or more SMS-SUBMIT PDUs.
pub fn encode_submit(req: &SubmitRequest) -> Result<EncodedMessage, PduError> {
    validate_common(req)?;
    let units = select_units(&req.payload, req.encoding, req.allow_unicode)?;
    let ports = port_ie(req.destination_port, req.source_port);
    let (concat, cap) = layout(&units, &ports);
    let chunks = split_units(units.alphabet, &units.data, cap);
    if chunks.len() > req.max_parts {
        return Err(PduError::TooLong(format!(
            "message needs {} parts, limit is {}",
            chunks.len(),
            req.max_parts
        )));
    }
    let da = encode_tp_address(&req.to)?;
    let smsc = encode_smsc(req.smsc.as_deref())?;
    let dcs = Dcs::encode(units.alphabet, req.message_class);
    let total = chunks.len() as u8;
    let mut parts = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let seq = i as u8 + 1;
        let mut udh = Vec::new();
        if concat {
            udh.extend([0x00, 0x03, req.reference, total, seq]);
        }
        udh.extend_from_slice(&ports);
        let (udl, ud) = build_user_data(units.alphabet, &udh, chunk);
        let mut fo = 0x01u8; // SMS-SUBMIT
        if req.reject_duplicates {
            fo |= 0x04;
        }
        if req.validity_minutes.is_some() {
            fo |= 0x10; // relative validity period
        }
        if req.status_report {
            fo |= 0x20;
        }
        if !udh.is_empty() {
            fo |= 0x40;
        }
        let mut tpdu = vec![fo, 0x00];
        tpdu.extend_from_slice(&da);
        tpdu.push(req.protocol_identifier);
        tpdu.push(dcs);
        if let Some(m) = req.validity_minutes {
            tpdu.push(encode_relative_validity(m));
        }
        tpdu.push(udl);
        tpdu.extend_from_slice(&ud);
        let mut full = smsc.clone();
        full.extend_from_slice(&tpdu);
        parts.push(EncodedPart {
            sequence: seq,
            pdu: hex::encode_upper(full),
            tpdu_length: tpdu.len(),
            text: decode_payload_text(units.alphabet, chunk, 0, 0),
            payload: chunk.to_vec(),
        });
    }
    Ok(EncodedMessage {
        encoding: units.alphabet,
        data_coding_scheme: dcs,
        reference: concat.then_some(req.reference),
        parts,
    })
}

/// Description of a synthetic SMS-DELIVER (simulator and tests).
#[derive(Debug, Clone)]
pub struct DeliverSpec {
    pub smsc: Option<String>,
    /// `+`/digit strings become numeric addresses, anything else alphanumeric.
    pub sender: String,
    pub payload: Payload,
    pub encoding: Encoding,
    pub message_class: Option<u8>,
    pub protocol_identifier: u8,
    pub timestamp: DateTime<FixedOffset>,
    pub destination_port: Option<u16>,
    pub source_port: Option<u16>,
    pub reference: u16,
    /// Use the 16-bit concatenation reference element.
    pub wide_reference: bool,
}

impl DeliverSpec {
    pub fn text(sender: &str, text: &str, timestamp: DateTime<FixedOffset>) -> Self {
        Self {
            smsc: Some("+4500000001".into()),
            sender: sender.into(),
            payload: Payload::Text(text.into()),
            encoding: Encoding::Auto,
            message_class: None,
            protocol_identifier: 0,
            timestamp,
            destination_port: None,
            source_port: None,
            reference: 0,
            wide_reference: false,
        }
    }
}

fn is_dialable(s: &str) -> bool {
    let d = s.strip_prefix('+').unwrap_or(s);
    !d.is_empty()
        && d.chars()
            .all(|c| c.is_ascii_digit() || c == '*' || c == '#')
}

/// Encode one or more SMS-DELIVER PDUs (modem format, with SMSC prefix).
pub fn encode_deliver(spec: &DeliverSpec) -> Result<Vec<String>, PduError> {
    let units = select_units(&spec.payload, spec.encoding, true)?;
    let ports = port_ie(spec.destination_port, spec.source_port);
    let concat_len = if spec.wide_reference { 6 } else { 5 };
    let single_udh = if ports.is_empty() { 0 } else { ports.len() + 1 };
    let (concat, cap) = if units.data.len() <= capacity(units.alphabet, single_udh) {
        (false, capacity(units.alphabet, single_udh))
    } else {
        (true, capacity(units.alphabet, ports.len() + concat_len + 1))
    };
    let chunks = split_units(units.alphabet, &units.data, cap);
    if chunks.len() > 255 {
        return Err(PduError::TooLong("more than 255 parts".into()));
    }
    let oa = if is_dialable(&spec.sender) {
        encode_tp_address(&spec.sender)?
    } else {
        encode_alphanumeric_address(&spec.sender)?
    };
    let smsc = encode_smsc(spec.smsc.as_deref())?;
    let dcs = Dcs::encode(units.alphabet, spec.message_class);
    let total = chunks.len() as u8;
    let mut out = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let seq = i as u8 + 1;
        let mut udh = Vec::new();
        if concat {
            if spec.wide_reference {
                udh.extend([0x08, 0x04]);
                udh.extend(spec.reference.to_be_bytes());
                udh.extend([total, seq]);
            } else {
                udh.extend([0x00, 0x03, spec.reference as u8, total, seq]);
            }
        }
        udh.extend_from_slice(&ports);
        let (udl, ud) = build_user_data(units.alphabet, &udh, chunk);
        let mut fo = 0x04u8; // SMS-DELIVER, no more messages
        if !udh.is_empty() {
            fo |= 0x40;
        }
        let mut pdu = smsc.clone();
        pdu.push(fo);
        pdu.extend_from_slice(&oa);
        pdu.push(spec.protocol_identifier);
        pdu.push(dcs);
        pdu.extend(Timestamp::encode(&spec.timestamp));
        pdu.push(udl);
        pdu.extend_from_slice(&ud);
        out.push(hex::encode_upper(pdu));
    }
    Ok(out)
}

/// Encode an SMS-STATUS-REPORT PDU (modem format, with SMSC prefix).
pub fn encode_status_report(
    smsc: Option<&str>,
    message_reference: u8,
    recipient: &str,
    submitted: DateTime<FixedOffset>,
    discharged: DateTime<FixedOffset>,
    status: u8,
) -> Result<String, PduError> {
    let mut pdu = encode_smsc(smsc)?;
    pdu.push(0x06); // SMS-STATUS-REPORT, no more messages
    pdu.push(message_reference);
    pdu.extend(encode_tp_address(recipient)?);
    pdu.extend(Timestamp::encode(&submitted));
    pdu.extend(Timestamp::encode(&discharged));
    pdu.push(status);
    Ok(hex::encode_upper(pdu))
}

#[cfg(test)]
mod tests {
    use super::super::decode::{decode, Pdu, PduDirection};
    use super::*;

    fn submit(hex: &str) -> super::super::decode::Submit {
        match decode(hex, PduDirection::MobileOriginated, true).unwrap() {
            Pdu::Submit(s) => *s,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn reference_submit() {
        // Widely published reference: "hellohello" to 46708251358, validity 4 days.
        let mut req = SubmitRequest::text("+46708251358", "hellohello");
        req.validity_minutes = Some(4 * 1440);
        let m = encode_submit(&req).unwrap();
        assert_eq!(m.parts.len(), 1);
        assert_eq!(
            m.parts[0].pdu,
            "0011000B916407281553F80000AA0AE8329BFD4697D9EC37"
        );
        assert_eq!(m.parts[0].tpdu_length, 23);
    }

    #[test]
    fn single_part_boundaries() {
        for (len, parts) in [(160usize, 1usize), (161, 2), (306, 2), (307, 3)] {
            let text = "a".repeat(len);
            let m = encode_submit(&SubmitRequest::text("+4500000000", &text)).unwrap();
            assert_eq!(m.parts.len(), parts, "gsm7 len {len}");
            let joined: String = m
                .parts
                .iter()
                .map(|p| submit(&p.pdu).user_data.text.unwrap())
                .collect();
            assert_eq!(joined, text);
        }
        for (len, parts) in [(70usize, 1usize), (71, 2), (134, 2), (135, 3)] {
            let text = "ж".repeat(len);
            let m = encode_submit(&SubmitRequest::text("+4500000000", &text)).unwrap();
            assert_eq!(m.encoding, Alphabet::Ucs2);
            assert_eq!(m.parts.len(), parts, "ucs2 len {len}");
            let joined: String = m
                .parts
                .iter()
                .map(|p| submit(&p.pdu).user_data.text.unwrap())
                .collect();
            assert_eq!(joined, text);
        }
    }

    #[test]
    fn concatenation_headers() {
        let text = "x".repeat(400);
        let mut req = SubmitRequest::text("+4500000000", &text);
        req.reference = 0x7B;
        req.status_report = true;
        let m = encode_submit(&req).unwrap();
        assert_eq!(m.parts.len(), 3);
        for (i, p) in m.parts.iter().enumerate() {
            let s = submit(&p.pdu);
            let c = s.user_data.header.unwrap().concatenation.unwrap();
            assert_eq!((c.reference, c.total, c.sequence), (0x7B, 3, i as u8 + 1));
            assert!(s.status_report_request);
            assert!(hex::decode(&p.pdu).unwrap().len() - 1 == p.tpdu_length);
            assert!(p.tpdu_length <= 164);
        }
    }

    #[test]
    fn never_splits_escape_or_surrogate() {
        // 152 plain septets then a euro sign (escape + 0x65) straddling the 153 boundary.
        let text = format!("{}€{}", "a".repeat(152), "b".repeat(20));
        let m = encode_submit(&SubmitRequest::text("+4500000000", &text)).unwrap();
        assert_eq!(m.parts[0].payload.len(), 152);
        let joined: String = m
            .parts
            .iter()
            .map(|p| submit(&p.pdu).user_data.text.unwrap())
            .collect();
        assert_eq!(joined, text);

        let text = format!("{}😀{}", "ж".repeat(66), "ж".repeat(5));
        let m = encode_submit(&SubmitRequest::text("+4500000000", &text)).unwrap();
        assert_eq!(m.parts[0].payload.len(), 132);
        let joined: String = m
            .parts
            .iter()
            .map(|p| submit(&p.pdu).user_data.text.unwrap())
            .collect();
        assert_eq!(joined, text);
    }

    #[test]
    fn binary_with_ports_and_class() {
        let mut req = SubmitRequest::text("+4500000000", "");
        req.payload = Payload::Binary((0..=200u8).collect());
        req.destination_port = Some(16000);
        req.source_port = Some(16001);
        req.message_class = Some(1);
        let m = encode_submit(&req).unwrap();
        assert_eq!(m.encoding, Alphabet::Binary);
        assert_eq!(m.parts.len(), 2);
        let mut all = vec![];
        for p in &m.parts {
            let s = submit(&p.pdu);
            let h = s.user_data.header.clone().unwrap();
            assert_eq!(h.ports.unwrap().destination, 16000);
            assert_eq!(s.data_coding_scheme.message_class, Some(1));
            all.extend(s.user_data.payload);
        }
        assert_eq!(all, (0..=200u8).collect::<Vec<_>>());
    }

    #[test]
    fn flash_and_validation() {
        let mut req = SubmitRequest::text("+4500000000", "flash");
        req.message_class = Some(0);
        let s = submit(&encode_submit(&req).unwrap().parts[0].pdu);
        assert_eq!(s.data_coding_scheme.message_class, Some(0));
        req.message_class = Some(7);
        assert!(encode_submit(&req).is_err());
        let mut long = SubmitRequest::text("+4500000000", &"a".repeat(2000));
        long.max_parts = 3;
        assert!(matches!(encode_submit(&long), Err(PduError::TooLong(_))));
        let mut uni = SubmitRequest::text("+4500000000", "привет");
        uni.allow_unicode = false;
        assert!(encode_submit(&uni).is_err());
        uni.allow_unicode = true;
        uni.encoding = Encoding::Gsm7;
        assert!(encode_submit(&uni).is_err());
        assert!(encode_submit(&SubmitRequest::text("not-a-number", "x")).is_err());
    }

    #[test]
    fn validity_round_trip() {
        use super::super::decode::relative_validity_minutes as dec;
        for m in [5u32, 60, 720, 750, 1440, 2880, 10080, 43200, 60480] {
            assert!(dec(encode_relative_validity(m)) >= m, "minutes {m}");
        }
        assert_eq!(encode_relative_validity(4 * 1440), 0xAA);
    }

    #[test]
    fn estimates() {
        let e = estimate(&Payload::Text("hello".into()), Encoding::Auto, true, false).unwrap();
        assert_eq!(
            (e.encoding, e.parts, e.units, e.units_remaining),
            (Alphabet::Gsm7, 1, 5, 155)
        );
        let e = estimate(&Payload::Text("€".repeat(100)), Encoding::Auto, true, false).unwrap();
        assert_eq!((e.parts, e.units, e.units_per_part), (2, 200, 153));
        let e = estimate(&Payload::Text("😀".into()), Encoding::Auto, true, false).unwrap();
        assert_eq!((e.encoding, e.units, e.characters), (Alphabet::Ucs2, 2, 1));
    }

    #[test]
    fn deliver_and_status_report_builders() {
        let ts = DateTime::parse_from_rfc3339("2025-01-02T03:04:05+01:00").unwrap();
        let mut spec = DeliverSpec::text("Example", &"long ".repeat(80), ts);
        spec.reference = 0x1234;
        spec.wide_reference = true;
        let pdus = encode_deliver(&spec).unwrap();
        assert_eq!(pdus.len(), 3);
        let mut text = String::new();
        for p in &pdus {
            let Pdu::Deliver(d) = decode(p, PduDirection::MobileTerminated, true).unwrap() else {
                panic!()
            };
            assert_eq!(d.sender.number, "Example");
            let c = d
                .user_data
                .header
                .as_ref()
                .unwrap()
                .concatenation
                .clone()
                .unwrap();
            assert!(c.wide_reference && c.reference == 0x1234);
            assert_eq!(d.timestamp.utc.unwrap(), ts);
            text.push_str(d.user_data.text.as_ref().unwrap());
        }
        assert_eq!(text, "long ".repeat(80));

        let sr =
            encode_status_report(Some("+4500000001"), 42, "+4500000000", ts, ts, 0x00).unwrap();
        let Pdu::StatusReport(r) = decode(&sr, PduDirection::MobileTerminated, true).unwrap()
        else {
            panic!()
        };
        assert_eq!((r.message_reference, r.status_category), (42, "delivered"));
        assert_eq!(r.recipient.number, "+4500000000");
    }
}
