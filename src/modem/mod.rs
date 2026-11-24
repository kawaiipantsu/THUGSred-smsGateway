//! Modem communication: serial transport, AT command engine, profiles,
//! capability detection and the per-modem supervisor state machine.

pub mod actor;
pub mod manager;
pub mod parser;
pub mod profiles;
pub mod simulator;
pub mod state;
pub mod transport;

pub use manager::{CallSignal, CallSignalKind, Modem, ModemDeps, PduSink, SendError};
pub use state::{ModemState, ModemStatus};
