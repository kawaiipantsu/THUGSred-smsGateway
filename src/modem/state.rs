//! Modem state machine states and the published status snapshot.

use crate::gnss::GnssFix;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ModemState {
    Initializing,
    Connecting,
    SimLocked,
    Registering,
    Ready,
    Degraded,
    Disconnected,
    Recovering,
    ShuttingDown,
    Disabled,
}

impl ModemState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Initializing => "INITIALIZING",
            Self::Connecting => "CONNECTING",
            Self::SimLocked => "SIM_LOCKED",
            Self::Registering => "REGISTERING",
            Self::Ready => "READY",
            Self::Degraded => "DEGRADED",
            Self::Disconnected => "DISCONNECTED",
            Self::Recovering => "RECOVERING",
            Self::ShuttingDown => "SHUTTING_DOWN",
            Self::Disabled => "DISABLED",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SimStatus {
    /// ready, pin_required, puk_required, missing, error or unknown
    pub state: String,
    /// SIM profile from sim.conf bound to this modem.
    pub profile: Option<String>,
    pub iccid: Option<String>,
    pub imsi: Option<String>,
    /// Subscriber number stored on the SIM, if any.
    pub msisdn: Option<String>,
    pub smsc: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct NetworkStatus {
    /// home, roaming, searching, denied, not_registered or unknown
    pub registration: String,
    pub registered: bool,
    pub operator: Option<String>,
    pub technology: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SignalStatus {
    pub raw: Option<u8>,
    pub rssi_dbm: Option<i32>,
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StorageStatus {
    pub name: Option<String>,
    pub used: Option<u32>,
    pub total: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AudioCapabilities {
    /// Audio paths the modem family offers according to its profile.
    pub modem_interfaces: Vec<String>,
    /// ALSA capture devices present on the host.
    pub host_capture_devices: Vec<String>,
    /// supported, unsupported or unverified
    pub call_recording: String,
}

/// Detected or profile-declared modem capabilities. Values are one of
/// `supported`, `unsupported`, `unverified` or `unknown`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Capabilities {
    pub detected: bool,
    pub sms_modes: Vec<String>,
    pub character_sets: Vec<String>,
    pub baud_rates: Vec<u32>,
    pub sms_storages: Vec<String>,
    pub delivery_report_routing: String,
    pub network_technologies: Vec<String>,
    pub voice: String,
    pub caller_id: String,
    pub ussd: String,
    pub gnss: String,
    pub mms: String,
    pub bluetooth: String,
    pub audio: AudioCapabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModemStatus {
    pub id: String,
    pub state: ModemState,
    pub state_since: String,
    pub enabled: bool,
    pub device: String,
    pub baud_rate: u32,
    pub profile: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub firmware: Option<String>,
    pub imei: Option<String>,
    pub sim: SimStatus,
    pub network: NetworkStatus,
    pub signal: SignalStatus,
    pub storage: StorageStatus,
    pub capabilities: Capabilities,
    pub gnss: Option<GnssFix>,
    pub gnss_last_fix_at: Option<String>,
    pub last_error: Option<String>,
    /// Actionable advice for the operator, e.g. to power the module on.
    pub hint: Option<String>,
    pub reconnect_attempts: u32,
    pub connected_at: Option<String>,
    pub last_seen_at: Option<String>,
}

impl ModemStatus {
    pub fn new(id: &str, device: &str, baud_rate: u32) -> Self {
        Self {
            id: id.to_string(),
            state: ModemState::Initializing,
            state_since: crate::utils::now(),
            enabled: true,
            device: device.to_string(),
            baud_rate,
            profile: None,
            manufacturer: None,
            model: None,
            firmware: None,
            imei: None,
            sim: SimStatus {
                state: "unknown".into(),
                ..Default::default()
            },
            network: NetworkStatus {
                registration: "unknown".into(),
                ..Default::default()
            },
            signal: SignalStatus::default(),
            storage: StorageStatus::default(),
            capabilities: Capabilities::default(),
            gnss: None,
            gnss_last_fix_at: None,
            last_error: None,
            hint: None,
            reconnect_attempts: 0,
            connected_at: None,
            last_seen_at: None,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state == ModemState::Ready
    }
}
