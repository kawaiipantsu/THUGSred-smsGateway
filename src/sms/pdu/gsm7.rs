//! GSM 7-bit default alphabet (3GPP TS 23.038) with extension and selected
//! national language shift tables, plus septet packing and unpacking.

use super::PduError;

pub const ESC: u8 = 0x1B;

/// GSM 7-bit default alphabet. Index is the septet value.
pub const BASIC: [char; 128] = [
    '@', '£', '$', '¥', 'è', 'é', 'ù', 'ì', 'ò', 'Ç', '\n', 'Ø', 'ø', '\r', 'Å', 'å', 'Δ', '_',
    'Φ', 'Γ', 'Λ', 'Ω', 'Π', 'Ψ', 'Σ', 'Θ', 'Ξ', '\u{1b}', 'Æ', 'æ', 'ß', 'É', ' ', '!', '"', '#',
    '¤', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/', '0', '1', '2', '3', '4', '5', '6',
    '7', '8', '9', ':', ';', '<', '=', '>', '?', '¡', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I',
    'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', 'Ä', 'Ö',
    'Ñ', 'Ü', '§', '¿', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o',
    'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'ä', 'ö', 'ñ', 'ü', 'à',
];

/// Default extension table (reached through the escape septet 0x1B).
const EXT_DEFAULT: &[(u8, char)] = &[
    (0x0A, '\u{0c}'),
    (0x14, '^'),
    (0x28, '{'),
    (0x29, '}'),
    (0x2F, '\\'),
    (0x3C, '['),
    (0x3D, '~'),
    (0x3E, ']'),
    (0x40, '|'),
    (0x65, '€'),
];

/// Turkish national language single shift table (identifier 1), additions only.
const EXT_TURKISH: &[(u8, char)] = &[
    (0x47, 'Ğ'),
    (0x49, 'İ'),
    (0x53, 'Ş'),
    (0x63, 'ç'),
    (0x67, 'ğ'),
    (0x69, 'ı'),
    (0x73, 'ş'),
];

/// Spanish national language single shift table (identifier 2), additions only.
const EXT_SPANISH: &[(u8, char)] = &[
    (0x09, 'ç'),
    (0x41, 'Á'),
    (0x49, 'Í'),
    (0x4F, 'Ó'),
    (0x55, 'Ú'),
    (0x61, 'á'),
    (0x69, 'í'),
    (0x6F, 'ó'),
    (0x75, 'ú'),
];

/// Portuguese national language single shift table (identifier 3), additions only.
const EXT_PORTUGUESE: &[(u8, char)] = &[
    (0x05, 'ê'),
    (0x09, 'ç'),
    (0x0B, 'Ô'),
    (0x0C, 'ô'),
    (0x0E, 'Á'),
    (0x0F, 'á'),
    (0x12, 'Φ'),
    (0x13, 'Γ'),
    (0x15, 'Ω'),
    (0x16, 'Π'),
    (0x17, 'Ψ'),
    (0x18, 'Σ'),
    (0x19, 'Θ'),
    (0x1F, 'Ê'),
    (0x41, 'À'),
    (0x49, 'Í'),
    (0x4F, 'Ó'),
    (0x55, 'Ú'),
    (0x5B, 'Ã'),
    (0x5C, 'Õ'),
    (0x61, 'Â'),
    (0x69, 'í'),
    (0x6F, 'ó'),
    (0x75, 'ú'),
    (0x7B, 'ã'),
    (0x7C, 'õ'),
    (0x7F, 'â'),
];

/// Turkish national language locking shift table (identifier 1): positions
/// that differ from the default alphabet.
const LOCK_TURKISH: &[(u8, char)] = &[
    (0x04, '€'),
    (0x07, 'ı'),
    (0x0B, 'Ğ'),
    (0x0C, 'ğ'),
    (0x1C, 'Ş'),
    (0x1D, 'ş'),
    (0x40, 'İ'),
    (0x60, 'ç'),
];

/// Returns true when the given national language identifier has a decoding table.
pub fn single_shift_supported(id: u8) -> bool {
    matches!(id, 0..=3)
}

pub fn locking_shift_supported(id: u8) -> bool {
    matches!(id, 0 | 1)
}

fn lookup(table: &[(u8, char)], septet: u8) -> Option<char> {
    table.iter().find(|(s, _)| *s == septet).map(|(_, c)| *c)
}

fn ext_char(septet: u8, single_shift: u8) -> Option<char> {
    let national = match single_shift {
        1 => lookup(EXT_TURKISH, septet),
        2 => lookup(EXT_SPANISH, septet),
        3 => lookup(EXT_PORTUGUESE, septet),
        _ => None,
    };
    national.or_else(|| lookup(EXT_DEFAULT, septet))
}

fn basic_char(septet: u8, locking_shift: u8) -> char {
    if locking_shift == 1 {
        if let Some(c) = lookup(LOCK_TURKISH, septet) {
            return c;
        }
    }
    BASIC[(septet & 0x7F) as usize]
}

/// Decode unpacked septets to a string using the given shift tables.
/// A dangling escape at the end of the input is dropped; an escape followed by
/// an unassigned code decodes to the basic-table character, as the
/// specification requires.
pub fn decode_septets(septets: &[u8], single_shift: u8, locking_shift: u8) -> String {
    let mut out = String::with_capacity(septets.len());
    let mut i = 0;
    while i < septets.len() {
        let s = septets[i] & 0x7F;
        if s == ESC {
            i += 1;
            match septets.get(i) {
                Some(&n) => {
                    let n = n & 0x7F;
                    match ext_char(n, single_shift) {
                        Some(c) => out.push(c),
                        None if n == ESC => out.push(' '),
                        None => out.push(basic_char(n, locking_shift)),
                    }
                }
                None => break,
            }
        } else {
            out.push(basic_char(s, locking_shift));
        }
        i += 1;
    }
    out
}

/// Encode a single character to one or two septets of the default alphabet.
pub fn encode_char(c: char) -> Option<(u8, Option<u8>)> {
    if c == '\u{1b}' {
        return None;
    }
    if let Some(pos) = BASIC.iter().position(|b| *b == c) {
        return Some((pos as u8, None));
    }
    EXT_DEFAULT
        .iter()
        .find(|(_, e)| *e == c)
        .map(|(s, _)| (ESC, Some(*s)))
}

/// Encode a string into unpacked septets. Returns `None` if any character is
/// outside the GSM default alphabet and its extension table.
pub fn encode_str(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        let (a, b) = encode_char(c)?;
        out.push(a);
        if let Some(b) = b {
            out.push(b);
        }
    }
    Some(out)
}

/// Whether every character can be represented in the GSM default alphabet.
pub fn is_gsm7(text: &str) -> bool {
    text.chars().all(|c| encode_char(c).is_some())
}

/// Pack septets into octets, starting `bit_offset` bits into the output
/// (used to align text after a user data header).
pub fn pack(septets: &[u8], bit_offset: usize) -> Vec<u8> {
    let total_bits = bit_offset + septets.len() * 7;
    let mut out = vec![0u8; total_bits.div_ceil(8)];
    let mut bit = bit_offset;
    for &s in septets {
        let s = (s & 0x7F) as u16;
        let byte = bit / 8;
        let shift = bit % 8;
        let v = s << shift;
        out[byte] |= (v & 0xFF) as u8;
        if shift > 1 {
            out[byte + 1] |= (v >> 8) as u8;
        }
        bit += 7;
    }
    out
}

/// Unpack `count` septets from `data` starting at `bit_offset`.
pub fn unpack(data: &[u8], bit_offset: usize, count: usize) -> Result<Vec<u8>, PduError> {
    let needed = bit_offset + count * 7;
    if needed > data.len() * 8 {
        return Err(PduError::Truncated("7-bit user data"));
    }
    let mut out = Vec::with_capacity(count);
    let mut bit = bit_offset;
    for _ in 0..count {
        let byte = bit / 8;
        let shift = bit % 8;
        let lo = (data[byte] as u16) >> shift;
        let hi = if shift > 1 {
            (*data.get(byte + 1).unwrap_or(&0) as u16) << (8 - shift)
        } else {
            0
        };
        out.push(((lo | hi) & 0x7F) as u8);
        bit += 7;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_hellohello() {
        let s = encode_str("hellohello").unwrap();
        assert_eq!(hex::encode_upper(pack(&s, 0)), "E8329BFD4697D9EC37");
        assert_eq!(unpack(&pack(&s, 0), 0, s.len()).unwrap(), s);
    }

    #[test]
    fn extension_characters_round_trip() {
        let text = "€[]{}~^|\\ price";
        let s = encode_str(text).unwrap();
        assert_eq!(s.len(), text.chars().count() + 9);
        assert_eq!(decode_septets(&s, 0, 0), text);
    }

    #[test]
    fn rejects_non_gsm() {
        assert!(encode_str("привет").is_none());
        assert!(!is_gsm7("emoji 😀"));
        assert!(is_gsm7("Ærø å @£$"));
    }

    #[test]
    fn offset_round_trip() {
        let s = encode_str("The quick brown fox").unwrap();
        for off in 0..7 {
            let packed = pack(&s, 48 + off);
            assert_eq!(unpack(&packed, 48 + off, s.len()).unwrap(), s);
        }
    }

    #[test]
    fn unpack_bounds_checked() {
        assert!(unpack(&[0xFF], 0, 2).is_err());
        assert!(unpack(&[], 0, 1).is_err());
    }

    #[test]
    fn dangling_escape_dropped() {
        assert_eq!(decode_septets(&[0x41, ESC], 0, 0), "A");
    }

    #[test]
    fn national_tables() {
        assert_eq!(decode_septets(&[ESC, 0x53], 1, 0), "Ş");
        assert_eq!(decode_septets(&[0x40], 0, 1), "İ");
        assert_eq!(decode_septets(&[ESC, 0x41], 2, 0), "Á");
        assert_eq!(decode_septets(&[ESC, 0x5B], 3, 0), "Ã");
    }
}
