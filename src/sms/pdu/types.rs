//! Shared PDU field types: addresses, data coding scheme, user data header,
//! timestamps and the bounds-checked reader.

use super::{gsm7, PduError};
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::Serialize;

pub(crate) mod hexser {
    use serde::Serializer;
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode_upper(v))
    }
}

/// Cursor over untrusted PDU bytes. Every read is bounds checked.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub fn u8(&mut self, what: &'static str) -> Result<u8, PduError> {
        let b = *self.buf.get(self.pos).ok_or(PduError::Truncated(what))?;
        self.pos += 1;
        Ok(b)
    }
    pub fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], PduError> {
        let end = self.pos.checked_add(n).ok_or(PduError::Truncated(what))?;
        let s = self
            .buf
            .get(self.pos..end)
            .ok_or(PduError::Truncated(what))?;
        self.pos = end;
        Ok(s)
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.buf[self.pos..];
        self.pos = self.buf.len();
        s
    }
    pub fn position(&self) -> usize {
        self.pos
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeOfNumber {
    Unknown,
    International,
    National,
    NetworkSpecific,
    Subscriber,
    Alphanumeric,
    Abbreviated,
    Reserved,
}

impl TypeOfNumber {
    pub fn from_toa(toa: u8) -> Self {
        match (toa >> 4) & 0x07 {
            0 => Self::Unknown,
            1 => Self::International,
            2 => Self::National,
            3 => Self::NetworkSpecific,
            4 => Self::Subscriber,
            5 => Self::Alphanumeric,
            6 => Self::Abbreviated,
            _ => Self::Reserved,
        }
    }
}

fn npi_name(toa: u8) -> &'static str {
    match toa & 0x0F {
        0 => "unknown",
        1 => "isdn",
        3 => "data",
        4 => "telex",
        8 => "national",
        9 => "private",
        10 => "ermes",
        _ => "reserved",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Address {
    /// Display form: `+` prefixed for international numbers, plain text for
    /// alphanumeric addresses.
    pub number: String,
    pub type_of_number: TypeOfNumber,
    pub numbering_plan: &'static str,
    pub type_of_address: u8,
}

const SEMI_DIGITS: [char; 15] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '*', '#', 'a', 'b', 'c',
];

impl Address {
    pub fn is_alphanumeric(&self) -> bool {
        self.type_of_number == TypeOfNumber::Alphanumeric
    }

    /// Build from the type-of-address octet and the address value octets.
    /// `semi_octets` is the number of useful semi-octets when known.
    pub fn from_parts(toa: u8, bytes: &[u8], semi_octets: Option<usize>) -> Result<Self, PduError> {
        let ton = TypeOfNumber::from_toa(toa);
        let number = if ton == TypeOfNumber::Alphanumeric {
            let bits = semi_octets.map(|s| s * 4).unwrap_or(bytes.len() * 8);
            let count = (bits / 7).min(bytes.len() * 8 / 7);
            let septets = gsm7::unpack(bytes, 0, count)?;
            gsm7::decode_septets(&septets, 0, 0)
        } else {
            let limit = semi_octets.unwrap_or(bytes.len() * 2);
            let mut s = String::with_capacity(limit + 1);
            if ton == TypeOfNumber::International {
                s.push('+');
            }
            let mut n = 0;
            'outer: for b in bytes {
                for nib in [b & 0x0F, b >> 4] {
                    if n >= limit || nib == 0x0F {
                        break 'outer;
                    }
                    s.push(SEMI_DIGITS[nib as usize]);
                    n += 1;
                }
            }
            s
        };
        Ok(Self {
            number,
            type_of_number: ton,
            numbering_plan: npi_name(toa),
            type_of_address: toa,
        })
    }
}

/// Encode a dialable number to (type-of-address, semi-octets, digit count).
fn encode_digits(number: &str) -> Result<(u8, Vec<u8>, usize), PduError> {
    let (toa, digits) = match number.strip_prefix('+') {
        Some(rest) => (0x91u8, rest),
        None => (0x81u8, number),
    };
    if digits.is_empty() || digits.len() > 20 {
        return Err(PduError::Invalid(
            "address must contain 1 to 20 digits".into(),
        ));
    }
    let mut nibbles = Vec::with_capacity(digits.len() + 1);
    for c in digits.chars() {
        // Only dialable characters are encoded; the semi-octet values for
        // 'a', 'b' and 'c' exist for decoding but are never generated.
        let n = SEMI_DIGITS[..12]
            .iter()
            .position(|d| *d == c)
            .ok_or_else(|| PduError::Invalid(format!("invalid character {c:?} in address")))?;
        nibbles.push(n as u8);
    }
    let count = nibbles.len();
    if count % 2 == 1 {
        nibbles.push(0x0F);
    }
    let bytes = nibbles.chunks(2).map(|p| p[0] | (p[1] << 4)).collect();
    Ok((toa, bytes, count))
}

/// Encode a TP-layer address (TP-DA / TP-OA / TP-RA).
pub fn encode_tp_address(number: &str) -> Result<Vec<u8>, PduError> {
    let (toa, bytes, count) = encode_digits(number)?;
    let mut out = vec![count as u8, toa];
    out.extend(bytes);
    Ok(out)
}

/// Encode an alphanumeric TP originating address (used by tests and the simulator).
pub fn encode_alphanumeric_address(text: &str) -> Result<Vec<u8>, PduError> {
    let septets = gsm7::encode_str(text)
        .ok_or_else(|| PduError::Invalid("alphanumeric address is not GSM 7-bit".into()))?;
    if septets.is_empty() || septets.len() > 11 {
        return Err(PduError::Invalid(
            "alphanumeric address must be 1 to 11 septets".into(),
        ));
    }
    let packed = gsm7::pack(&septets, 0);
    let semi = (septets.len() * 7).div_ceil(4);
    let mut out = vec![semi as u8, 0xD0];
    out.extend(packed);
    Ok(out)
}

/// Encode the service centre address prefix. `None` yields `00`, which makes
/// the modem use the SMSC stored on the SIM.
pub fn encode_smsc(number: Option<&str>) -> Result<Vec<u8>, PduError> {
    match number {
        None | Some("") => Ok(vec![0]),
        Some(n) => {
            let (toa, bytes, _) = encode_digits(n)?;
            let mut out = vec![(bytes.len() + 1) as u8, toa];
            out.extend(bytes);
            Ok(out)
        }
    }
}

pub fn read_tp_address(r: &mut Reader<'_>, what: &'static str) -> Result<Address, PduError> {
    let len = r.u8(what)? as usize;
    if len > 20 {
        return Err(PduError::Invalid(format!(
            "{what}: address length {len} exceeds 20 semi-octets"
        )));
    }
    let toa = r.u8(what)?;
    let bytes = r.take(len.div_ceil(2), what)?;
    Address::from_parts(toa, bytes, Some(len))
}

pub fn read_smsc(r: &mut Reader<'_>) -> Result<Option<Address>, PduError> {
    let len = r.u8("SMSC length")? as usize;
    if len == 0 {
        return Ok(None);
    }
    if len > 12 {
        return Err(PduError::Invalid(format!(
            "SMSC length {len} exceeds 12 octets"
        )));
    }
    let toa = r.u8("SMSC type")?;
    let bytes = r.take(len - 1, "SMSC address")?;
    Address::from_parts(toa, bytes, None).map(Some)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Alphabet {
    Gsm7,
    #[serde(rename = "8bit")]
    Binary,
    Ucs2,
}

impl Alphabet {
    pub fn as_str(&self) -> &'static str {
        match self {
            Alphabet::Gsm7 => "gsm7",
            Alphabet::Binary => "8bit",
            Alphabet::Ucs2 => "ucs2",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessageWaiting {
    pub active: bool,
    /// voicemail, fax, email or other
    pub kind: &'static str,
    pub store: bool,
}

/// Interpreted TP-DCS (3GPP TS 23.038 section 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dcs {
    pub raw: u8,
    pub alphabet: Alphabet,
    pub message_class: Option<u8>,
    pub compressed: bool,
    pub auto_delete: bool,
    pub message_waiting: Option<MessageWaiting>,
    pub reserved: bool,
}

impl Dcs {
    pub fn parse(raw: u8) -> Self {
        let mut d = Dcs {
            raw,
            alphabet: Alphabet::Gsm7,
            message_class: None,
            compressed: false,
            auto_delete: false,
            message_waiting: None,
            reserved: false,
        };
        let alpha = |bits: u8, reserved: &mut bool| match bits {
            0 => Alphabet::Gsm7,
            1 => Alphabet::Binary,
            2 => Alphabet::Ucs2,
            _ => {
                *reserved = true;
                Alphabet::Gsm7
            }
        };
        match raw >> 4 {
            0x0..=0x7 => {
                d.auto_delete = raw & 0x40 != 0;
                d.compressed = raw & 0x20 != 0;
                d.alphabet = alpha((raw >> 2) & 0x03, &mut d.reserved);
                if raw & 0x10 != 0 {
                    d.message_class = Some(raw & 0x03);
                }
            }
            0x8..=0xB => d.reserved = true,
            g @ 0xC..=0xE => {
                d.alphabet = if g == 0xE {
                    Alphabet::Ucs2
                } else {
                    Alphabet::Gsm7
                };
                d.message_waiting = Some(MessageWaiting {
                    active: raw & 0x08 != 0,
                    kind: match raw & 0x03 {
                        0 => "voicemail",
                        1 => "fax",
                        2 => "email",
                        _ => "other",
                    },
                    store: g != 0xC,
                });
            }
            _ => {
                d.alphabet = if raw & 0x04 != 0 {
                    Alphabet::Binary
                } else {
                    Alphabet::Gsm7
                };
                d.message_class = Some(raw & 0x03);
            }
        }
        d
    }

    /// Build a general-group DCS octet.
    pub fn encode(alphabet: Alphabet, class: Option<u8>) -> u8 {
        let a = match alphabet {
            Alphabet::Gsm7 => 0u8,
            Alphabet::Binary => 1,
            Alphabet::Ucs2 => 2,
        } << 2;
        match class {
            Some(c) => 0x10 | a | (c & 0x03),
            None => a,
        }
    }
}

/// Human-readable interpretation of TP-PID (3GPP TS 23.040 section 9.2.3.9).
pub fn pid_description(pid: u8) -> &'static str {
    match pid {
        0x00 => "normal",
        0x01..=0x1F => "reserved",
        0x20..=0x3F => "telematic_interworking",
        0x40 => "short_message_type_0",
        0x41..=0x47 => "replace_short_message",
        0x48 => "device_triggering",
        0x5E => "enhanced_message_service",
        0x5F => "return_call",
        0x7C => "ansi_136_r_data",
        0x7D => "me_data_download",
        0x7E => "me_depersonalization",
        0x7F => "sim_data_download",
        0x49..=0x7B => "reserved",
        0x80..=0xBF => "reserved",
        _ => "sc_specific",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Concatenation {
    pub reference: u16,
    pub total: u8,
    pub sequence: u8,
    /// True for the 16-bit reference information element (IEI 0x08).
    pub wide_reference: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ports {
    pub destination: u16,
    pub source: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InformationElement {
    pub id: u8,
    #[serde(with = "hexser")]
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Udh {
    pub concatenation: Option<Concatenation>,
    pub ports: Option<Ports>,
    pub national_single_shift: Option<u8>,
    pub national_locking_shift: Option<u8>,
    pub elements: Vec<InformationElement>,
}

impl Udh {
    /// Parse the information elements of a user data header (without the
    /// leading UDHL octet). Malformed trailing elements produce a warning and
    /// are skipped instead of failing the whole message.
    pub fn parse(mut data: &[u8], warnings: &mut Vec<String>) -> Self {
        let mut udh = Udh::default();
        while !data.is_empty() {
            if data.len() < 2 {
                warnings.push("truncated information element in user data header".into());
                break;
            }
            let (id, len) = (data[0], data[1] as usize);
            let Some(body) = data.get(2..2 + len) else {
                warnings.push(format!(
                    "information element 0x{id:02X} exceeds user data header"
                ));
                break;
            };
            match (id, len) {
                (0x00, 3) if body[1] != 0 && body[2] != 0 => {
                    udh.concatenation = Some(Concatenation {
                        reference: body[0] as u16,
                        total: body[1],
                        sequence: body[2],
                        wide_reference: false,
                    })
                }
                (0x08, 4) if body[2] != 0 && body[3] != 0 => {
                    udh.concatenation = Some(Concatenation {
                        reference: u16::from_be_bytes([body[0], body[1]]),
                        total: body[2],
                        sequence: body[3],
                        wide_reference: true,
                    })
                }
                (0x04, 2) => {
                    udh.ports = Some(Ports {
                        destination: body[0] as u16,
                        source: body[1] as u16,
                    })
                }
                (0x05, 4) => {
                    udh.ports = Some(Ports {
                        destination: u16::from_be_bytes([body[0], body[1]]),
                        source: u16::from_be_bytes([body[2], body[3]]),
                    })
                }
                (0x24, 1) => udh.national_single_shift = Some(body[0]),
                (0x25, 1) => udh.national_locking_shift = Some(body[0]),
                (0x00 | 0x08 | 0x04 | 0x05 | 0x24 | 0x25, _) => warnings.push(format!(
                    "information element 0x{id:02X} has invalid content and was ignored"
                )),
                _ => {}
            }
            udh.elements.push(InformationElement {
                id,
                data: body.to_vec(),
            });
            data = &data[2 + len..];
        }
        udh
    }
}

/// Service centre time stamp (TP-SCTS) or discharge time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Timestamp {
    /// Raw digits as `YY/MM/DD,hh:mm:ss±zz` (zz in quarter hours).
    pub raw: String,
    /// RFC 3339 timestamp with the network-provided offset, when valid.
    pub local: Option<String>,
    pub utc: Option<DateTime<Utc>>,
}

fn swap(b: u8) -> u32 {
    ((b & 0x0F) as u32) * 10 + (b >> 4) as u32
}

impl Timestamp {
    pub fn parse(b: &[u8]) -> Self {
        debug_assert_eq!(b.len(), 7);
        let (yy, mo, dd, hh, mi, ss) = (
            swap(b[0]),
            swap(b[1]),
            swap(b[2]),
            swap(b[3]),
            swap(b[4]),
            swap(b[5]),
        );
        let negative = b[6] & 0x08 != 0;
        let quarters = ((b[6] & 0x07) as i32) * 10 + (b[6] >> 4) as i32;
        let sign = if negative { '-' } else { '+' };
        let raw = format!("{yy:02}/{mo:02}/{dd:02},{hh:02}:{mi:02}:{ss:02}{sign}{quarters:02}");
        let offset_secs = quarters * 900 * if negative { -1 } else { 1 };
        let dt: Option<DateTime<FixedOffset>> =
            NaiveDate::from_ymd_opt(if yy >= 90 { 1900 } else { 2000 } + yy as i32, mo, dd)
                .and_then(|d| d.and_hms_opt(hh, mi, ss))
                .and_then(|n| {
                    FixedOffset::east_opt(offset_secs)
                        .and_then(|off| n.and_local_timezone(off).single())
                });
        Timestamp {
            raw,
            local: dt.map(|d| d.to_rfc3339()),
            utc: dt.map(|d| d.with_timezone(&Utc)),
        }
    }

    /// Encode a timestamp into the 7-octet semi-octet representation.
    pub fn encode(dt: &DateTime<FixedOffset>) -> [u8; 7] {
        use chrono::{Datelike, Timelike};
        let sw = |v: u32| (((v % 10) << 4) | (v / 10 % 10)) as u8;
        let off = dt.offset().local_minus_utc();
        let quarters = (off.unsigned_abs() / 900).min(79);
        let mut tz = sw(quarters);
        if off < 0 {
            tz |= 0x08;
        }
        [
            sw(dt.year().rem_euclid(100) as u32),
            sw(dt.month()),
            sw(dt.day()),
            sw(dt.hour()),
            sw(dt.minute()),
            sw(dt.second()),
            tz,
        ]
    }
}

/// Decoded TP-User-Data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserData {
    /// TP-UDL as transmitted (septets for GSM 7-bit, octets otherwise).
    pub length: u8,
    pub encoding: Alphabet,
    pub header: Option<Udh>,
    /// Decoded text for GSM 7-bit and UCS-2 payloads.
    pub text: Option<String>,
    /// Payload without the user data header: one unpacked septet per byte for
    /// GSM 7-bit, raw octets otherwise. Multipart assembly concatenates this.
    #[serde(with = "hexser")]
    pub payload: Vec<u8>,
    pub warnings: Vec<String>,
}

pub fn decode_ucs2(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Decode payload units (as stored in [`UserData::payload`]) into text.
pub fn decode_payload_text(
    encoding: Alphabet,
    payload: &[u8],
    single: u8,
    locking: u8,
) -> Option<String> {
    match encoding {
        Alphabet::Gsm7 => Some(gsm7::decode_septets(payload, single, locking)),
        Alphabet::Ucs2 => Some(decode_ucs2(payload)),
        Alphabet::Binary => None,
    }
}

pub fn decode_user_data(udhi: bool, dcs: &Dcs, udl: u8, ud: &[u8]) -> Result<UserData, PduError> {
    let mut warnings = Vec::new();
    let udl_n = udl as usize;
    let octets = match dcs.alphabet {
        Alphabet::Gsm7 if !dcs.compressed => (udl_n * 7).div_ceil(8),
        _ => udl_n,
    };
    if octets > 140 {
        return Err(PduError::Invalid(format!(
            "user data length {udl} exceeds 140 octets"
        )));
    }
    if ud.len() < octets {
        return Err(PduError::Truncated("user data"));
    }
    if ud.len() > octets {
        warnings.push(format!(
            "{} trailing octet(s) after user data ignored",
            ud.len() - octets
        ));
    }
    let ud = &ud[..octets];
    let mut header = None;
    let mut header_len = 0usize;
    if udhi {
        let udhl = *ud.first().ok_or(PduError::Truncated("user data header"))? as usize;
        let body = ud
            .get(1..1 + udhl)
            .ok_or(PduError::Truncated("user data header"))?;
        header = Some(Udh::parse(body, &mut warnings));
        header_len = udhl + 1;
    }
    if dcs.reserved {
        warnings.push("reserved data coding scheme; decoded as GSM 7-bit".into());
    }
    let (single, locking) = header
        .as_ref()
        .map(|h| {
            (
                h.national_single_shift.unwrap_or(0),
                h.national_locking_shift.unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));
    let (payload, text) = if dcs.compressed {
        warnings.push("compressed user data is not supported; payload kept raw".into());
        (ud[header_len..].to_vec(), None)
    } else {
        match dcs.alphabet {
            Alphabet::Gsm7 => {
                let header_septets = (header_len * 8).div_ceil(7);
                if header_septets > udl_n {
                    return Err(PduError::Invalid(
                        "user data header longer than user data".into(),
                    ));
                }
                let septets = gsm7::unpack(ud, header_septets * 7, udl_n - header_septets)?;
                if !gsm7::single_shift_supported(single) || !gsm7::locking_shift_supported(locking)
                {
                    warnings.push(format!(
                        "national language table (single {single}, locking {locking}) not supported; default alphabet used"
                    ));
                }
                let text = gsm7::decode_septets(&septets, single, locking);
                (septets, Some(text))
            }
            Alphabet::Ucs2 => {
                let p = &ud[header_len..];
                if p.len() % 2 != 0 {
                    warnings.push("odd UCS-2 payload length; last octet ignored".into());
                }
                (p.to_vec(), Some(decode_ucs2(p)))
            }
            Alphabet::Binary => (ud[header_len..].to_vec(), None),
        }
    };
    Ok(UserData {
        length: udl,
        encoding: dcs.alphabet,
        header,
        text,
        payload,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_round_trip() {
        let enc = encode_tp_address("+4512345678").unwrap();
        assert_eq!(hex::encode_upper(&enc), "0A915421436587");
        let mut r = Reader::new(&enc);
        let a = read_tp_address(&mut r, "t").unwrap();
        assert_eq!(a.number, "+4512345678");
        assert_eq!(a.type_of_number, TypeOfNumber::International);
        let odd = encode_tp_address("12345").unwrap();
        assert_eq!(hex::encode_upper(&odd), "05812143F5");
        let a = read_tp_address(&mut Reader::new(&odd), "t").unwrap();
        assert_eq!(a.number, "12345");
    }

    #[test]
    fn alphanumeric_address() {
        let enc = encode_alphanumeric_address("Example").unwrap();
        let a = read_tp_address(&mut Reader::new(&enc), "t").unwrap();
        assert_eq!(a.number, "Example");
        assert!(a.is_alphanumeric());
    }

    #[test]
    fn rejects_bad_addresses() {
        assert!(encode_tp_address("").is_err());
        assert!(encode_tp_address("+12ab;").is_err());
        assert!(encode_tp_address("abc").is_err());
        assert!(encode_tp_address("123456789012345678901").is_err());
        assert!(read_tp_address(&mut Reader::new(&[0x30, 0x91, 0x00]), "t").is_err());
        assert!(read_tp_address(&mut Reader::new(&[0x0A, 0x91, 0x00]), "t").is_err());
    }

    #[test]
    fn dcs_groups() {
        assert_eq!(Dcs::parse(0x00).alphabet, Alphabet::Gsm7);
        assert_eq!(Dcs::parse(0x08).alphabet, Alphabet::Ucs2);
        assert_eq!(Dcs::parse(0x04).alphabet, Alphabet::Binary);
        assert_eq!(Dcs::parse(0x10).message_class, Some(0));
        assert_eq!(Dcs::parse(0xF5).message_class, Some(1));
        assert_eq!(Dcs::parse(0xF5).alphabet, Alphabet::Binary);
        let mwi = Dcs::parse(0xC8).message_waiting.unwrap();
        assert!(mwi.active && !mwi.store && mwi.kind == "voicemail");
        assert!(Dcs::parse(0x80).reserved);
        assert_eq!(Dcs::encode(Alphabet::Ucs2, None), 0x08);
        assert_eq!(Dcs::encode(Alphabet::Gsm7, Some(0)), 0x10);
    }

    #[test]
    fn timestamp_with_offsets() {
        // 2024-03-15 14:30:45 +01:00 (4 quarters)
        let t = Timestamp::parse(&[0x42, 0x30, 0x51, 0x41, 0x03, 0x54, 0x40]);
        assert_eq!(t.raw, "24/03/15,14:30:45+04");
        assert_eq!(t.utc.unwrap().to_rfc3339(), "2024-03-15T13:30:45+00:00");
        // negative offset: -05:00 = 20 quarters, sign bit set
        let t = Timestamp::parse(&[0x42, 0x30, 0x51, 0x41, 0x03, 0x54, 0x0A]);
        assert_eq!(t.utc.unwrap().to_rfc3339(), "2024-03-15T19:30:45+00:00");
        // invalid month does not panic
        let t = Timestamp::parse(&[0x42, 0x99, 0x51, 0x41, 0x03, 0x54, 0x40]);
        assert!(t.utc.is_none());
        let dt = DateTime::parse_from_rfc3339("2024-03-15T14:30:45-05:00").unwrap();
        assert_eq!(Timestamp::parse(&Timestamp::encode(&dt)).utc.unwrap(), dt);
    }

    #[test]
    fn udh_parsing_is_tolerant() {
        let mut w = vec![];
        let u = Udh::parse(
            &[
                0x00, 0x03, 0xAB, 0x02, 0x01, 0x05, 0x04, 0x0B, 0x84, 0x23, 0xF0,
            ],
            &mut w,
        );
        assert_eq!(u.concatenation.as_ref().unwrap().reference, 0xAB);
        assert_eq!(u.ports.as_ref().unwrap().destination, 2948);
        assert!(w.is_empty());
        let u = Udh::parse(&[0x00, 0x09, 0x01], &mut w);
        assert!(u.concatenation.is_none());
        assert_eq!(w.len(), 1);
    }
}
