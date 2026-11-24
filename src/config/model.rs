//! Configuration data model. Every field has a default so that a minimal
//! configuration file is valid; see `docs/CONFIGURATION.md` for the reference.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CONFIG_VERSION: u32 = 1;

macro_rules! config_struct {
    ($(#[$m:meta])* pub struct $name:ident { $( $(#[$fm:meta])* pub $f:ident : $t:ty = $d:expr ),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct $name { $( $(#[$fm])* pub $f: $t ),* }
        impl Default for $name {
            fn default() -> Self { Self { $( $f: $d ),* } }
        }
    };
}

fn s(v: &str) -> String {
    v.to_string()
}

config_struct! {
pub struct General {
    pub instance_name: String = s("THUGSred SMS Gateway"),
    pub instance_id: String = s("gateway-01"),
    pub timezone: String = s("UTC"),
    pub log_level: String = s("info"),
    pub shutdown_timeout_seconds: u64 = 30,
}}

config_struct! {
pub struct Paths {
    pub home: String = s("/opt/THUGSred-smsGateway"),
    pub data: String = s("/var/lib/thugsred-smsgateway"),
    pub runtime: String = s("/run/thugsred-smsgateway"),
    pub recordings: String = s("/opt/THUGSred-smsGateway/recordings"),
    pub attachments: String = s("/var/lib/thugsred-smsgateway/attachments"),
    pub backups: String = s("/opt/THUGSred-smsGateway/backups"),
    pub modem_profiles: String = s("/etc/thugsred/modem-profiles"),
    pub sim_config: String = s("/etc/thugsred/sim.conf"),
    /// Address book: telephone numbers and the names to show for them.
    pub addressbook: String = s("/etc/thugsred/addressbook.conf"),
    /// Numbers that are always anonymised in integration output.
    pub anonymous: String = s("/etc/thugsred/anonymous.conf"),
}}

config_struct! {
/// Settings of one modem. The `[modem]` table provides the defaults; each
/// `[[modems]]` entry overrides them for one physical modem.
pub struct ModemConfig {
    pub id: String = s("modem-1"),
    /// Identifier of the SIM profile in sim.conf bound to this modem.
    pub sim: String = String::new(),
    pub enabled: bool = true,
    pub profile: String = s("auto"),
    pub device: String = s("/dev/ttyS0"),
    pub baud_rate: u32 = 115200,
    pub data_bits: u8 = 8,
    pub stop_bits: u8 = 1,
    pub parity: String = s("none"),
    pub flow_control: String = s("none"),
    pub command_timeout_seconds: u64 = 10,
    pub message_timeout_seconds: u64 = 60,
    pub poll_interval_seconds: u64 = 15,
    pub healthcheck_interval_seconds: u64 = 30,
    pub reconnect_delay_seconds: u64 = 5,
    pub max_reconnect_delay_seconds: u64 = 300,
    pub auto_reconnect: bool = true,
    pub auto_detect_capabilities: bool = true,
    pub preferred_network: String = s("auto"),
    /// Extra AT commands issued after the profile's own initialisation.
    pub init_commands: Vec<String> = Vec::new(),
    /// Preferred SMS storage (for example "SM" or "ME"); empty keeps the profile default.
    pub sms_storage: String = String::new(),
}}

config_struct! {
pub struct Sms {
    pub enabled: bool = true,
    pub mode: String = s("pdu"),
    pub receive_enabled: bool = true,
    pub send_enabled: bool = true,
    pub auto_encoding: bool = true,
    pub default_encoding: String = s("gsm7"),
    pub allow_unicode: bool = true,
    pub allow_multipart: bool = true,
    pub allow_binary: bool = true,
    pub allow_flash_sms: bool = true,
    pub allow_service_messages: bool = true,
    pub request_delivery_reports: bool = true,
    pub store_raw_pdu: bool = true,
    pub store_sent_messages: bool = true,
    pub delete_from_modem_after_persist: bool = true,
    pub multipart_timeout_seconds: u64 = 600,
    pub max_message_parts: usize = 10,
    pub max_send_attempts: u32 = 3,
    pub send_retry_delay_seconds: u64 = 10,
    pub outbound_rate_limit_per_minute: u32 = 10,
    pub deduplication_enabled: bool = true,
    /// Modem that sends messages which do not name one; empty lets any ready modem take them.
    pub default_modem: String = String::new(),
    /// Minimum seconds between two messages to the same destination (0 disables).
    pub per_destination_interval_seconds: u64 = 0,
    /// Default TP-Validity-Period in minutes (0 lets the network decide).
    pub default_validity_minutes: u32 = 0,
    /// Queued messages not submitted within this time expire (0 disables).
    pub queue_expiry_seconds: u64 = 86400,
    /// Days to wait for a delivery report before correlation stops.
    pub delivery_report_window_days: u32 = 7,
}}

config_struct! {
pub struct Mms {
    pub enabled: bool = false,
    pub provider_profile: String = s("auto"),
    pub apn: String = String::new(),
    pub mmsc_url: String = String::new(),
    pub proxy: String = String::new(),
    pub download_attachments: bool = false,
    pub max_attachment_size_mb: u64 = 10,
}}

config_struct! {
pub struct Ussd {
    pub enabled: bool = true,
    pub timeout_seconds: u64 = 30,
    /// Allow API callers to run USSD codes that are not listed in `commands`.
    pub allow_arbitrary_codes: bool = false,
    /// Named USSD codes, for example `balance = "*101#"`.
    pub commands: BTreeMap<String, String> = BTreeMap::new(),
}}

config_struct! {
pub struct Database {
    pub driver: String = s("sqlite"),
    pub sqlite_path: String = s("/var/lib/thugsred-smsgateway/messages.db"),
    pub mysql_url: String = String::new(),
    pub max_connections: u32 = 5,
    pub auto_migrate: bool = true,
    pub sqlite_wal: bool = true,
}}

config_struct! {
pub struct Retention {
    pub message_days: u32 = 0,
    pub recording_days: u32 = 30,
    pub attachment_days: u32 = 30,
    pub integration_event_days: u32 = 30,
    pub audit_days: u32 = 90,
    pub cleanup_interval_seconds: u64 = 3600,
}}

config_struct! {
pub struct Api {
    pub enabled: bool = true,
    pub bind: String = s("0.0.0.0"),
    pub port: u16 = 8080,
    /// "none" or "bearer"
    pub authentication: String = s("none"),
    pub tokens: Vec<String> = Vec::new(),
    pub allowed_cidrs: Vec<String> = vec![
        s("127.0.0.0/8"), s("::1/128"), s("10.0.0.0/8"), s("172.16.0.0/12"), s("192.168.0.0/16"),
    ],
    /// Reverse proxies whose X-Forwarded-For header is trusted.
    pub trusted_proxies: Vec<String> = Vec::new(),
    /// Accepted Host header values; empty disables Host validation.
    pub allowed_hosts: Vec<String> = Vec::new(),
    pub cors_enabled: bool = false,
    pub cors_allowed_origins: Vec<String> = Vec::new(),
    pub max_request_size_kb: usize = 256,
    pub request_timeout_seconds: u64 = 30,
    pub rate_limit_per_minute: u32 = 120,
    /// Administrative endpoints (reconnect, delete, retry, integration tests, raw AT).
    pub admin_enabled: bool = true,
    /// Additional CIDR restriction for administrative endpoints; empty reuses `allowed_cidrs`.
    pub admin_allowed_cidrs: Vec<String> = Vec::new(),
    /// Bearer tokens required for administrative endpoints; empty reuses `authentication`.
    pub admin_tokens: Vec<String> = Vec::new(),
}}

config_struct! {
pub struct Web {
    pub enabled: bool = true,
    pub title: String = s("THUGSred SMS Gateway"),
    pub theme: String = s("dark"),
    pub refresh_interval_seconds: u64 = 10,
    pub show_message_content: bool = true,
    pub show_modem_information: bool = true,
}}

config_struct! {
pub struct Logging {
    pub system_log: String = s("/var/log/thugsred-smsgateway/system.log"),
    pub messages_log: String = s("/var/log/thugsred-smsgateway/messages.log"),
    pub integrations_log: String = s("/var/log/thugsred-smsgateway/integrations.log"),
    pub audit_log: String = s("/var/log/thugsred-smsgateway/audit.log"),
    pub format: String = s("json"),
    pub journal_enabled: bool = true,
    pub rotation: String = s("daily"),
    pub retention_days: u32 = 30,
    pub log_message_content: bool = false,
    pub redact_sensitive_values: bool = true,
}}

config_struct! {
pub struct Calls {
    pub enabled: bool = true,
    pub log_incoming: bool = true,
    pub log_outgoing: bool = true,
    pub auto_answer: bool = false,
    pub recording_enabled: bool = false,
    pub recording_duration_seconds: u64 = 15,
    pub audio_backend: String = s("auto"),
    pub audio_device: String = String::new(),
    pub recording_format: String = s("wav"),
    pub hangup_after_recording: bool = true,
    /// Seconds without a RING before an unanswered call is logged as missed.
    pub ring_timeout_seconds: u64 = 8,
}}

config_struct! {
pub struct Gnss {
    pub enabled: bool = false,
    pub poll_interval_seconds: u64 = 60,
    pub store_location_history: bool = false,
    /// Modem that provides GNSS; empty selects the first modem.
    pub modem: String = String::new(),
}}

config_struct! {
pub struct IntegrationSettings {
    pub enabled: bool = true,
    pub config_file: String = s("/etc/thugsred/integrations.conf"),
    pub worker_count: usize = 4,
    pub persistent_queue: bool = true,
    pub max_attempts: u32 = 10,
    pub retry_initial_seconds: u64 = 5,
    pub retry_max_seconds: u64 = 3600,
}}

config_struct! {
pub struct Metrics {
    pub enabled: bool = true,
    pub path: String = s("/metrics"),
}}

config_struct! {
pub struct Security {
    pub allow_raw_at_commands: bool = false,
    pub allow_raw_pdu_submission: bool = false,
    /// Allow administrative callers to bypass `destination_allowlist` per request.
    pub allow_destination_overrides: bool = false,
    pub max_outbound_queue_size: u64 = 10000,
    /// When non-empty, outbound SMS is only accepted for matching destinations
    /// (exact numbers or prefixes ending in `*`).
    pub destination_allowlist: Vec<String> = Vec::new(),
    pub destination_blocklist: Vec<String> = Vec::new(),
}}

config_struct! {
pub struct SimNetwork {
    pub selection: String = s("automatic"),
    pub operator_code: String = String::new(),
    pub roaming_enabled: bool = false,
    pub apn: String = String::new(),
    pub username: String = String::new(),
    pub password: String = String::new(),
}}

config_struct! {
pub struct SimIdentity {
    pub manufacturer: String = String::new(),
    pub model: String = String::new(),
    pub asset_id: String = String::new(),
    pub location: String = String::new(),
    pub notes: String = String::new(),
}}

config_struct! {
pub struct SimLimits {
    pub monthly_sms_limit: u64 = 0,
    pub daily_sms_limit: u64 = 0,
}}

config_struct! {
/// Descriptive SIM metadata and credentials. `mobile_number` is gateway
/// metadata only; the network decides the originating number.
pub struct SimProfile {
    pub id: String = s("default"),
    pub name: String = String::new(),
    pub owner: String = String::new(),
    pub description: String = String::new(),
    pub mobile_number: String = String::new(),
    pub country_code: String = String::new(),
    pub carrier: String = String::new(),
    pub pin: String = String::new(),
    pub pin_enabled: bool = false,
    pub pin_max_attempts: u32 = 1,
    pub smsc: String = String::new(),
    pub iccid: String = String::new(),
    pub imsi: String = String::new(),
    pub network: SimNetwork = SimNetwork::default(),
    pub identity: SimIdentity = SimIdentity::default(),
    pub limits: SimLimits = SimLimits::default(),
}}

config_struct! {
/// One address book entry.
pub struct Contact {
    pub name: String = String::new(),
    pub number: String = String::new(),
}}

config_struct! {
/// Numbers that must never be disclosed to integrations (anonymous.conf).
pub struct Anonymous {
    /// Exact numbers or prefixes ending in `*`.
    pub numbers: Vec<String> = Vec::new(),
    /// Text shown instead of an anonymised number.
    pub label: String = s("Anonymous"),
    /// Also withhold the message text of anonymised numbers from integrations.
    pub redact_body: bool = false,
}}

config_struct! {
/// Conditions an event must satisfy. Empty lists and strings match anything.
pub struct Filter {
    pub sender_allowlist: Vec<String> = Vec::new(),
    pub sender_blocklist: Vec<String> = Vec::new(),
    pub recipient_allowlist: Vec<String> = Vec::new(),
    pub recipient_blocklist: Vec<String> = Vec::new(),
    pub sender_regex: String = String::new(),
    pub message_regex: String = String::new(),
    pub message_contains: Vec<String> = Vec::new(),
    pub directions: Vec<String> = Vec::new(),
    pub message_types: Vec<String> = Vec::new(),
    pub message_classes: Vec<i64> = Vec::new(),
    pub application_ports: Vec<i64> = Vec::new(),
    pub encodings: Vec<String> = Vec::new(),
    pub instance_ids: Vec<String> = Vec::new(),
    pub modem_ids: Vec<String> = Vec::new(),
    pub sim_ids: Vec<String> = Vec::new(),
    pub delivery_statuses: Vec<String> = Vec::new(),
}}

config_struct! {
pub struct IntegrationDef {
    pub name: String = String::new(),
    #[serde(rename = "type")]
    pub kind: String = s("webhook"),
    pub enabled: bool = false,
    /// Event names; `*` and prefixes such as `sms.*` are accepted.
    pub events: Vec<String> = Vec::new(),
    pub url: String = String::new(),
    pub webhook_url: String = String::new(),
    pub method: String = s("POST"),
    pub content_type: String = s("application/json"),
    pub timeout_seconds: u64 = 10,
    pub verify_tls: bool = true,
    pub max_attempts: u32 = 0,
    pub retry_initial_seconds: u64 = 0,
    pub retry_max_seconds: u64 = 0,
    /// Deliveries per minute for this integration (0 = unlimited).
    pub rate_limit_per_minute: u32 = 0,
    pub include_raw_pdu: bool = false,
    /// Replace every telephone number (and address book name) sent through
    /// this integration with the anonymous label.
    pub anonymize: bool = false,
    /// When false, message text is replaced by a placeholder for this integration.
    pub include_message_body: bool = true,
    pub sign_payload: bool = false,
    pub signing_secret: String = String::new(),
    pub headers: BTreeMap<String, String> = BTreeMap::new(),
    pub query: BTreeMap<String, String> = BTreeMap::new(),
    /// Custom request body template for webhooks (overrides the JSON event envelope).
    pub body_template: String = String::new(),
    /// Message text template for chat style integrations.
    pub template: String = String::new(),
    pub username: String = String::new(),
    pub bot_token: String = String::new(),
    pub chat_id: String = String::new(),
    /// Access token (ntfy, Gotify).
    pub token: String = String::new(),
    /// ntfy topic.
    pub topic: String = String::new(),
    pub priority: i64 = 0,
    /// Syslog server as host:port (UDP).
    pub server: String = String::new(),
    pub facility: String = s("local0"),
    pub filter: Filter = Filter::default(),
}}

config_struct! {
pub struct RoutingRule {
    pub name: String = String::new(),
    pub enabled: bool = true,
    pub events: Vec<String> = Vec::new(),
    pub destinations: Vec<String> = Vec::new(),
    pub stop_processing: bool = false,
    #[serde(flatten)]
    pub filter: Filter = Filter::default(),
}}

config_struct! {
/// Opt-in SMS automation: automatic replies and SMS forwarding.
pub struct AutomationRule {
    pub name: String = String::new(),
    pub enabled: bool = false,
    /// "reply" or "forward"
    pub action: String = s("reply"),
    /// Forward destinations (required for `forward`).
    pub to: Vec<String> = Vec::new(),
    pub template: String = String::new(),
    /// Modem used to send; empty reuses the receiving modem.
    pub modem: String = String::new(),
    /// Minimum seconds between two automatic messages triggered by the same sender.
    pub cooldown_seconds: u64 = 3600,
    /// Upper bound of automatic messages per hour for this rule.
    pub max_per_hour: u32 = 10,
    #[serde(flatten)]
    pub filter: Filter = Filter::default(),
}}

/// Fully resolved runtime configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Config {
    pub config_version: u32,
    pub general: General,
    pub paths: Paths,
    /// Defaults applied to every modem.
    pub modem: ModemConfig,
    /// Effective per-modem settings (at least one entry).
    pub modems: Vec<ModemConfig>,
    pub sms: Sms,
    pub mms: Mms,
    pub ussd: Ussd,
    pub database: Database,
    pub retention: Retention,
    pub api: Api,
    pub web: Web,
    pub logging: Logging,
    pub calls: Calls,
    pub gnss: Gnss,
    pub integrations: IntegrationSettings,
    pub metrics: Metrics,
    pub security: Security,
    pub sims: Vec<SimProfile>,
    pub integration: Vec<IntegrationDef>,
    pub routing: Vec<RoutingRule>,
    pub automation: Vec<AutomationRule>,
    pub contacts: Vec<Contact>,
    pub anonymous: Anonymous,
    /// Non-fatal findings from loading (unknown keys, risky settings).
    #[serde(skip)]
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub files: ConfigFiles,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConfigFiles {
    pub main: String,
    pub sim: String,
    pub integrations: String,
    pub addressbook: String,
    pub anonymous: String,
}

impl Config {
    pub fn modem(&self, id: &str) -> Option<&ModemConfig> {
        self.modems.iter().find(|m| m.id == id)
    }
    pub fn sim(&self, id: &str) -> Option<&SimProfile> {
        self.sims.iter().find(|s| s.id == id)
    }
    /// SIM profile bound to a modem by configuration.
    pub fn sim_for_modem(&self, modem_id: &str) -> Option<&SimProfile> {
        let m = self.modem(modem_id)?;
        if !m.sim.is_empty() {
            return self.sim(&m.sim);
        }
        if self.modems.len() == 1 && self.sims.len() == 1 {
            return self.sims.first();
        }
        None
    }
    /// Address book name for a number, if known.
    pub fn contact_name(&self, number: &str) -> Option<&str> {
        if number.is_empty() {
            return None;
        }
        self.contacts
            .iter()
            .find(|c| crate::utils::list_matches(&c.number, number))
            .or_else(|| {
                self.contacts
                    .iter()
                    .find(|c| crate::utils::numbers_match(&c.number, number))
            })
            .map(|c| c.name.as_str())
    }
    /// Whether a number is on the always-anonymise list.
    pub fn is_anonymous(&self, number: &str) -> bool {
        !number.is_empty()
            && self.anonymous.numbers.iter().any(|p| {
                crate::utils::list_matches(p, number)
                    || (!p.ends_with('*') && crate::utils::numbers_match(p, number))
            })
    }
    pub fn integration_def(&self, name: &str) -> Option<&IntegrationDef> {
        self.integration.iter().find(|i| i.name == name)
    }
}
