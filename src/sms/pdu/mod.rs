//! SMS PDU codec covering the relevant parts of 3GPP TS 23.040 and 23.038.
//!
//! Everything in this module is pure and operates on untrusted input with
//! strict bounds checking; nothing here talks to a modem.

pub mod decode;
pub mod encode;
pub mod gsm7;
pub mod types;

pub use decode::{decode, decode_auto, Deliver, Pdu, PduDirection, StatusReport, Submit};
pub use encode::{
    encode_deliver, encode_status_report, encode_submit, estimate, DeliverSpec, EncodedMessage,
    EncodedPart, Encoding, Estimate, Payload, SubmitRequest,
};
pub use types::{Address, Alphabet, Dcs, Timestamp, Udh, UserData};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PduError {
    #[error("PDU is not valid hexadecimal")]
    InvalidHex,
    #[error("PDU truncated while reading {0}")]
    Truncated(&'static str),
    #[error("invalid PDU: {0}")]
    Invalid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("message too long: {0}")]
    TooLong(String),
}

/// Parse a hexadecimal PDU string, tolerating surrounding whitespace.
pub fn parse_hex(s: &str) -> Result<Vec<u8>, PduError> {
    let t = s.trim();
    if t.is_empty() || t.len() % 2 != 0 || t.len() > 1024 {
        return Err(PduError::InvalidHex);
    }
    hex::decode(t).map_err(|_| PduError::InvalidHex)
}
