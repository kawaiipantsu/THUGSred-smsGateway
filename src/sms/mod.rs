//! SMS subsystem: PDU codec, inbound processing, outbound queue and automation.

mod automation;
mod inbound;
pub mod ota;
pub mod pdu;
pub mod sender;
mod service;

pub use service::{quota_windows, SendRequest, SmsService, SubmitError, Submitted};
