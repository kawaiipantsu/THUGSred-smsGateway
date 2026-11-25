//! Configuration loading, validation, redaction and reload support.

mod model;
pub use model::*;

use arc_swap::ArcSwap;
use regex::Regex;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toml::Value;

pub type SharedConfig = Arc<ArcSwap<Config>>;

pub const ENV_PREFIX: &str = "THUGSRED_SMSGW__";

/// All event names the gateway can emit.
pub const KNOWN_EVENTS: &[&str] = &[
    "sms.received",
    "sms.queued",
    "sms.sent",
    "sms.delivered",
    "sms.failed",
    "sms.multipart_completed",
    "sms.binary_received",
    "sms.service_received",
    "sms.ota_toolkit",
    "security.sim_attack",
    "security.threat_detected",
    "mms.notification",
    "mms.received",
    "ussd.response",
    "call.incoming",
    "call.missed",
    "call.ended",
    "call.recorded",
    "modem.connected",
    "modem.disconnected",
    "modem.error",
    "modem.signal_changed",
    "sim.ready",
    "sim.locked",
    "network.registered",
    "network.lost",
    "gnss.fix",
    "gateway.started",
    "gateway.stopped",
    "gateway.error",
    "gateway.config_reloaded",
    "integration.test",
];

pub const INTEGRATION_TYPES: &[&str] = &[
    "webhook", "discord", "slack", "teams", "telegram", "ntfy", "gotify", "syslog",
];

#[derive(Debug, thiserror::Error)]
#[error("configuration is invalid:\n{}", .errors.iter().map(|e| format!("  - {e}")).collect::<Vec<_>>().join("\n"))]
pub struct ConfigError {
    pub errors: Vec<String>,
}

impl ConfigError {
    fn one(msg: impl Into<String>) -> Self {
        Self {
            errors: vec![msg.into()],
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    pub main_path: PathBuf,
    /// `section.field=value` overrides from the command line.
    pub overrides: Vec<String>,
    /// Apply `THUGSRED_SMSGW__SECTION__FIELD` environment overrides.
    pub use_env: bool,
}

impl LoadOptions {
    pub fn new(main_path: impl Into<PathBuf>) -> Self {
        Self {
            main_path: main_path.into(),
            overrides: Vec::new(),
            use_env: true,
        }
    }
}

fn read_toml(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: cannot read file: {e}", path.display()))?;
    parse_toml(&text, &path.display().to_string())
}

fn parse_toml(text: &str, name: &str) -> Result<Value, String> {
    text.parse::<toml::Table>()
        .map(Value::Table)
        .map_err(|e| format!("{name}: {}", e.to_string().trim_end()))
}

/// Parse an override value as a TOML literal, falling back to a plain string.
fn parse_literal(raw: &str) -> Value {
    format!("v = {raw}")
        .parse::<toml::Table>()
        .ok()
        .and_then(|mut t| t.remove("v"))
        .unwrap_or_else(|| Value::String(raw.to_string()))
}

fn set_path(root: &mut Value, path: &[String], value: Value) -> Result<(), String> {
    let mut cur = root;
    for (i, key) in path.iter().enumerate() {
        let table = cur
            .as_table_mut()
            .ok_or_else(|| format!("{} is not a table", path[..i].join(".")))?;
        if i == path.len() - 1 {
            table.insert(key.clone(), value);
            return Ok(());
        }
        cur = table
            .entry(key.clone())
            .or_insert_with(|| Value::Table(Default::default()));
    }
    Ok(())
}

fn apply_overrides(root: &mut Value, opts: &LoadOptions, errors: &mut Vec<String>) {
    if opts.use_env {
        let mut vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with(ENV_PREFIX))
            .collect();
        vars.sort();
        for (k, v) in vars {
            let path: Vec<String> = k[ENV_PREFIX.len()..]
                .split("__")
                .map(|p| p.to_ascii_lowercase())
                .collect();
            if path.iter().any(|p| p.is_empty()) {
                errors.push(format!("environment override {k}: malformed name"));
                continue;
            }
            if let Err(e) = set_path(root, &path, parse_literal(&v)) {
                errors.push(format!("environment override {k}: {e}"));
            }
        }
    }
    for o in &opts.overrides {
        match o.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() => {
                let path: Vec<String> = k.trim().split('.').map(str::to_string).collect();
                if let Err(e) = set_path(root, &path, parse_literal(v.trim())) {
                    errors.push(format!("override {k}: {e}"));
                }
            }
            _ => errors.push(format!("override {o:?}: expected section.field=value")),
        }
    }
}

/// Variables from the optional `smsgateway.env` file next to the main
/// configuration (the file systemd loads through `EnvironmentFile=`). The
/// process environment takes precedence; the file lets command-line tools run
/// by an administrator resolve the same `${NAME}` references as the service.
type EnvFile = std::collections::HashMap<String, String>;

fn read_env_file(main: &Path) -> EnvFile {
    let mut out = EnvFile::new();
    let Some(dir) = main.parent() else { return out };
    let Ok(text) = std::fs::read_to_string(dir.join("smsgateway.env")) else {
        return out;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
                .unwrap_or(v);
            out.insert(k.trim().to_string(), v.to_string());
        }
    }
    out
}

/// Replace `${ENV_VAR}` and `${file:/path}` references in every string.
fn resolve_secrets(
    v: &mut Value,
    path: &str,
    lenient: bool,
    env_file: &EnvFile,
    errors: &mut Vec<String>,
) {
    match v {
        Value::String(s) if s.contains("${") => {
            let re = Regex::new(r"\$\{(file:)?([^}]+)\}").expect("static regex");
            let mut failed = false;
            let out = re
                .replace_all(s, |c: &regex::Captures<'_>| {
                    let name = &c[2];
                    let res = if c.get(1).is_some() {
                        std::fs::read_to_string(name)
                            .map(|t| t.trim_end_matches(['\r', '\n']).to_string())
                            .map_err(|e| format!("{path}: cannot read secret file {name}: {e}"))
                    } else {
                        std::env::var(name)
                            .ok()
                            .or_else(|| env_file.get(name).cloned())
                            .ok_or_else(|| {
                                format!("{path}: environment variable {name} is not set (and not found in smsgateway.env, which may need root to read)")
                            })
                    };
                    match res {
                        Ok(v) => v,
                        Err(e) => {
                            failed = true;
                            if !lenient {
                                errors.push(e);
                            }
                            String::new()
                        }
                    }
                })
                .into_owned();
            *s = if failed { String::new() } else { out };
        }
        Value::Table(t) => {
            for (k, child) in t.iter_mut() {
                resolve_secrets(child, &format!("{path}.{k}"), lenient, env_file, errors);
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter_mut().enumerate() {
                // Disabled entries (integrations, rules) may reference secrets
                // that are not provisioned yet.
                let disabled = child
                    .get("enabled")
                    .map(|e| e.as_bool() == Some(false))
                    .unwrap_or(child.get("name").is_some() && child.get("type").is_some());
                resolve_secrets(
                    child,
                    &format!("{path}[{i}]"),
                    lenient || disabled,
                    env_file,
                    errors,
                );
            }
        }
        _ => {}
    }
}

/// Report keys present in the input that the parsed structure does not know.
fn unknown_keys(input: &Value, parsed: &Json, path: &str, out: &mut Vec<String>) {
    match (input, parsed) {
        (Value::Table(t), Json::Object(o)) => {
            for (k, v) in t {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match o.get(k) {
                    Some(child) => unknown_keys(v, child, &p, out),
                    None => out.push(p),
                }
            }
        }
        (Value::Array(a), Json::Array(b)) => {
            for (i, (v, child)) in a.iter().zip(b).enumerate() {
                unknown_keys(v, child, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

fn section<T: DeserializeOwned + Default + serde::Serialize>(
    root: &Value,
    key: &str,
    file: &str,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> T {
    let Some(v) = root.get(key) else {
        return T::default();
    };
    from_value(v, &format!("{file}: [{key}]"), key, errors, warnings)
}

fn from_value<T: DeserializeOwned + Default + serde::Serialize>(
    v: &Value,
    label: &str,
    path: &str,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> T {
    match v.clone().try_into::<T>() {
        Ok(t) => {
            let mut unknown = Vec::new();
            if let Ok(j) = serde_json::to_value(&t) {
                unknown_keys(v, &j, path, &mut unknown);
            }
            for u in unknown {
                warnings.push(format!("unknown configuration key `{u}` ignored"));
            }
            t
        }
        Err(e) => {
            errors.push(format!("{label}: {}", e.to_string().trim_end()));
            T::default()
        }
    }
}

const MAIN_SECTIONS: &[&str] = &[
    "config_version",
    "general",
    "paths",
    "modem",
    "modems",
    "sms",
    "mms",
    "ussd",
    "database",
    "retention",
    "api",
    "web",
    "logging",
    "calls",
    "gnss",
    "integrations",
    "metrics",
    "security",
];

/// Load and validate the complete configuration.
pub fn load(opts: &LoadOptions) -> Result<Config, ConfigError> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let main_name = opts.main_path.display().to_string();
    let mut root = read_toml(&opts.main_path).map_err(ConfigError::one)?;
    apply_overrides(&mut root, opts, &mut errors);
    let env_file = read_env_file(&opts.main_path);
    resolve_secrets(&mut root, "smsgateway.conf", false, &env_file, &mut errors);

    let mut cfg = Config {
        config_version: root
            .get("config_version")
            .and_then(Value::as_integer)
            .unwrap_or(CONFIG_VERSION as i64) as u32,
        ..Default::default()
    };
    if cfg.config_version != CONFIG_VERSION {
        errors.push(format!(
            "config_version: version {} is not supported by this release (expected {CONFIG_VERSION})",
            cfg.config_version
        ));
    }
    if let Some(t) = root.as_table() {
        for k in t.keys() {
            if !MAIN_SECTIONS.contains(&k.as_str()) {
                warnings.push(format!("unknown configuration section `{k}` ignored"));
            }
        }
    }
    let (e, w) = (&mut errors, &mut warnings);
    cfg.general = section(&root, "general", &main_name, e, w);
    cfg.paths = section(&root, "paths", &main_name, e, w);
    cfg.modem = section(&root, "modem", &main_name, e, w);
    cfg.sms = section(&root, "sms", &main_name, e, w);
    cfg.mms = section(&root, "mms", &main_name, e, w);
    cfg.ussd = section(&root, "ussd", &main_name, e, w);
    cfg.database = section(&root, "database", &main_name, e, w);
    cfg.retention = section(&root, "retention", &main_name, e, w);
    cfg.api = section(&root, "api", &main_name, e, w);
    cfg.web = section(&root, "web", &main_name, e, w);
    cfg.logging = section(&root, "logging", &main_name, e, w);
    cfg.calls = section(&root, "calls", &main_name, e, w);
    cfg.gnss = section(&root, "gnss", &main_name, e, w);
    cfg.integrations = section(&root, "integrations", &main_name, e, w);
    cfg.metrics = section(&root, "metrics", &main_name, e, w);
    cfg.security = section(&root, "security", &main_name, e, w);

    // Per-modem settings: [[modems]] entries layered over [modem].
    let base = root
        .get("modem")
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default();
    match root.get("modems") {
        None => cfg.modems = vec![cfg.modem.clone()],
        Some(Value::Array(list)) if !list.is_empty() => {
            for (i, entry) in list.iter().enumerate() {
                let Some(t) = entry.as_table() else {
                    errors.push(format!("{main_name}: modems[{i}] must be a table"));
                    continue;
                };
                let mut merged = base.clone();
                merged.remove("id");
                merged.remove("sim");
                for (k, v) in t {
                    merged.insert(k.clone(), v.clone());
                }
                if !merged.contains_key("id") {
                    merged.insert("id".into(), Value::String(format!("modem-{}", i + 1)));
                }
                cfg.modems.push(from_value(
                    &Value::Table(merged),
                    &format!("{main_name}: modems[{i}]"),
                    &format!("modems[{i}]"),
                    &mut errors,
                    &mut warnings,
                ));
            }
        }
        Some(_) => errors.push(format!(
            "{main_name}: modems must be a non-empty array of tables ([[modems]])"
        )),
    }

    // SIM configuration.
    let sim_path = PathBuf::from(&cfg.paths.sim_config);
    cfg.files = ConfigFiles {
        main: main_name.clone(),
        sim: cfg.paths.sim_config.clone(),
        integrations: cfg.integrations.config_file.clone(),
        addressbook: cfg.paths.addressbook.clone(),
        anonymous: cfg.paths.anonymous.clone(),
        providers: cfg.paths.providers.clone(),
        branding: cfg.paths.branding.clone(),
    };
    if sim_path.exists() {
        check_permissions(&sim_path, &mut warnings);
        match read_toml(&sim_path) {
            Ok(mut v) => {
                resolve_secrets(&mut v, "sim.conf", false, &env_file, &mut errors);
                let file = cfg.paths.sim_config.clone();
                load_sims(&v, &file, &mut cfg, &mut errors, &mut warnings);
            }
            Err(e) => errors.push(e),
        }
    } else {
        warnings.push(format!(
            "SIM configuration {} not found; using defaults",
            sim_path.display()
        ));
    }
    if cfg.sims.is_empty() {
        cfg.sims.push(SimProfile::default());
    }

    // Integrations, routing and automation.
    let int_path = PathBuf::from(&cfg.integrations.config_file);
    if int_path.exists() {
        match read_toml(&int_path) {
            Ok(mut v) => {
                resolve_secrets(&mut v, "integrations.conf", false, &env_file, &mut errors);
                let file = cfg.integrations.config_file.clone();
                load_integrations(&v, &file, &mut cfg, &mut errors, &mut warnings);
            }
            Err(e) => errors.push(e),
        }
    } else {
        warnings.push(format!(
            "integrations configuration {} not found; no integrations loaded",
            int_path.display()
        ));
    }

    // Upstream SMS providers (optional).
    let prov_path = PathBuf::from(&cfg.paths.providers);
    if prov_path.exists() {
        check_permissions(&prov_path, &mut warnings);
        match read_toml(&prov_path) {
            Ok(mut v) => {
                resolve_secrets(&mut v, "providers.conf", false, &env_file, &mut errors);
                let file = cfg.paths.providers.clone();
                if let Some(t) = v.as_table() {
                    for k in t.keys().filter(|k| k.as_str() != "provider") {
                        warnings.push(format!("{file}: unknown section `{k}` ignored"));
                    }
                }
                match v.get("provider") {
                    None => {}
                    Some(Value::Array(list)) => {
                        for (i, entry) in list.iter().enumerate() {
                            cfg.providers.push(from_value(
                                entry,
                                &format!("{file}: provider[{i}]"),
                                &format!("provider[{i}]"),
                                &mut errors,
                                &mut warnings,
                            ));
                        }
                    }
                    Some(_) => errors.push(format!(
                        "{file}: provider must be an array of tables ([[provider]])"
                    )),
                }
            }
            Err(e) => errors.push(e),
        }
    }

    // Dashboard branding (optional).
    let brand_path = PathBuf::from(&cfg.paths.branding);
    if brand_path.exists() {
        match read_toml(&brand_path) {
            Ok(v) => {
                let label = format!("{}: branding", cfg.paths.branding);
                cfg.branding = from_value(&v, &label, "branding", &mut errors, &mut warnings);
            }
            Err(e) => errors.push(e),
        }
    }

    // Address book and always-anonymous list (both optional).
    let book_path = PathBuf::from(&cfg.paths.addressbook);
    if book_path.exists() {
        match read_toml(&book_path) {
            Ok(v) => load_addressbook(
                &v,
                &cfg.paths.addressbook.clone(),
                &mut cfg,
                &mut errors,
                &mut warnings,
            ),
            Err(e) => errors.push(e),
        }
    }
    let anon_path = PathBuf::from(&cfg.paths.anonymous);
    if anon_path.exists() {
        check_permissions(&anon_path, &mut warnings);
        match read_toml(&anon_path) {
            Ok(v) => {
                let label = format!("{}: anonymous list", cfg.paths.anonymous);
                cfg.anonymous = from_value(&v, &label, "anonymous", &mut errors, &mut warnings);
            }
            Err(e) => errors.push(e),
        }
    }

    validate(&cfg, &mut errors, &mut warnings);
    cfg.warnings = warnings;
    if errors.is_empty() {
        Ok(cfg)
    } else {
        Err(ConfigError { errors })
    }
}

fn check_permissions(path: &Path, warnings: &mut Vec<String>) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.permissions().mode() & 0o007 != 0 {
            warnings.push(format!(
                "{} is accessible by other users; restrict it with chmod 640",
                path.display()
            ));
        }
    }
}

fn load_sims(
    v: &Value,
    file: &str,
    cfg: &mut Config,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if let Some(t) = v.as_table() {
        for k in t.keys() {
            if !["sim", "network", "identity", "limits", "sims"].contains(&k.as_str()) {
                warnings.push(format!(
                    "{file}: unknown configuration section `{k}` ignored"
                ));
            }
        }
    }
    if let Some(Value::Array(list)) = v.get("sims") {
        for (i, entry) in list.iter().enumerate() {
            let mut sim: SimProfile = from_value(
                entry,
                &format!("{file}: sims[{i}]"),
                &format!("sims[{i}]"),
                errors,
                warnings,
            );
            if entry.get("id").is_none() {
                sim.id = format!("sim-{}", i + 1);
            }
            cfg.sims.push(sim);
        }
    }
    if v.get("sim").is_some() {
        // Classic single-SIM layout: [sim], [network], [identity], [limits].
        let mut t = v
            .get("sim")
            .and_then(Value::as_table)
            .cloned()
            .unwrap_or_default();
        for k in ["network", "identity", "limits"] {
            if let Some(sub) = v.get(k) {
                t.insert(k.into(), sub.clone());
            }
        }
        let sim: SimProfile = from_value(
            &Value::Table(t),
            &format!("{file}: [sim]"),
            "sim",
            errors,
            warnings,
        );
        cfg.sims.insert(0, sim);
    }
}

fn load_addressbook(
    v: &Value,
    file: &str,
    cfg: &mut Config,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if let Some(t) = v.as_table() {
        for k in t.keys() {
            if !["contacts", "contact"].contains(&k.as_str()) {
                warnings.push(format!(
                    "{file}: unknown configuration section `{k}` ignored"
                ));
            }
        }
    }
    // Compact form: [contacts] "+4500000000" = "Name"
    if let Some(t) = v.get("contacts").and_then(Value::as_table) {
        for (number, name) in t {
            match name.as_str() {
                Some(n) => cfg.contacts.push(Contact {
                    name: n.to_string(),
                    number: number.clone(),
                    ..Default::default()
                }),
                None => errors.push(format!(
                    "{file}: contacts.{number:?} must be a string (the name to show)"
                )),
            }
        }
    }
    // Table form: [[contact]] name = "...", number = "..."
    if let Some(Value::Array(list)) = v.get("contact") {
        for (i, entry) in list.iter().enumerate() {
            cfg.contacts.push(from_value(
                entry,
                &format!("{file}: contact[{i}]"),
                &format!("contact[{i}]"),
                errors,
                warnings,
            ));
        }
    }
    // Entries of the file are never "saved" contacts, whatever they claim.
    for c in &mut cfg.contacts {
        c.source = "file".into();
        c.id.clear();
    }
    for c in &cfg.contacts {
        if c.name.trim().is_empty() || c.number.trim().is_empty() {
            errors.push(format!("{file}: every contact needs a name and a number"));
            break;
        }
    }
}

fn load_integrations(
    v: &Value,
    file: &str,
    cfg: &mut Config,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if let Some(t) = v.as_table() {
        for k in t.keys() {
            if !["integration", "routing", "automation"].contains(&k.as_str()) {
                warnings.push(format!(
                    "{file}: unknown configuration section `{k}` ignored"
                ));
            }
        }
    }
    if let Some(Value::Array(list)) = v.get("integration") {
        for (i, entry) in list.iter().enumerate() {
            cfg.integration.push(from_value(
                entry,
                &format!("{file}: integration[{i}]"),
                &format!("integration[{i}]"),
                errors,
                warnings,
            ));
        }
    }
    if let Some(Value::Array(list)) = v.get("routing").and_then(|r| r.get("rule")) {
        for (i, entry) in list.iter().enumerate() {
            cfg.routing.push(from_value(
                entry,
                &format!("{file}: routing.rule[{i}]"),
                &format!("routing.rule[{i}]"),
                errors,
                warnings,
            ));
        }
    }
    if let Some(Value::Array(list)) = v.get("automation").and_then(|r| r.get("rule")) {
        for (i, entry) in list.iter().enumerate() {
            cfg.automation.push(from_value(
                entry,
                &format!("{file}: automation.rule[{i}]"),
                &format!("automation.rule[{i}]"),
                errors,
                warnings,
            ));
        }
    }
}

fn one_of(errors: &mut Vec<String>, field: &str, value: &str, allowed: &[&str]) {
    if !allowed.contains(&value) {
        errors.push(format!(
            "{field}: invalid value {value:?} (expected one of: {})",
            allowed.join(", ")
        ));
    }
}

fn positive(errors: &mut Vec<String>, field: &str, value: u64) {
    if value == 0 {
        errors.push(format!("{field}: must be greater than zero"));
    }
}

/// Does `pattern` (exact name, `*` or `prefix.*`) cover event `name`?
pub fn event_matches(pattern: &str, name: &str) -> bool {
    pattern == "*"
        || pattern == name
        || pattern
            .strip_suffix(".*")
            .map(|p| name.starts_with(p) && name[p.len()..].starts_with('.'))
            .unwrap_or(false)
}

fn validate_events(errors: &mut Vec<String>, field: &str, events: &[String]) {
    for e in events {
        if !KNOWN_EVENTS.iter().any(|k| event_matches(e, k)) {
            errors.push(format!("{field}: unknown event {e:?}"));
        }
    }
}

fn validate_filter(errors: &mut Vec<String>, field: &str, f: &Filter) {
    for (name, re) in [
        ("sender_regex", &f.sender_regex),
        ("message_regex", &f.message_regex),
    ] {
        if !re.is_empty() {
            if let Err(e) = regex::RegexBuilder::new(re).size_limit(1 << 20).build() {
                errors.push(format!("{field}.{name}: invalid regular expression: {e}"));
            }
        }
    }
    for d in &f.directions {
        one_of(
            errors,
            &format!("{field}.directions"),
            d,
            &["inbound", "outbound"],
        );
    }
}

fn validate_cidrs(errors: &mut Vec<String>, field: &str, list: &[String]) {
    for c in list {
        if c.parse::<ipnet::IpNet>().is_err() && c.parse::<std::net::IpAddr>().is_err() {
            errors.push(format!("{field}: {c:?} is not a valid CIDR or IP address"));
        }
    }
}

fn valid_http_url(u: &str) -> bool {
    reqwest::Url::parse(u)
        .map(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
        .unwrap_or(false)
}

/// Validate a dialable destination number.
pub fn valid_number(n: &str) -> bool {
    let d = n.strip_prefix('+').unwrap_or(n);
    !d.is_empty() && d.len() <= 20 && d.chars().all(|c| c.is_ascii_digit())
}

/// Check a provider sender: 3 to 11 letters, digits or spaces containing at
/// least one letter, or a number of 3 to 15 digits.
pub fn valid_sender_name(sender: &str) -> Result<(), String> {
    let n = sender.chars().count();
    if sender.chars().all(|c| c.is_ascii_digit()) {
        return if (3..=15).contains(&n) {
            Ok(())
        } else {
            Err("a numeric sender must have 3 to 15 digits".into())
        };
    }
    if !sender
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ')
    {
        return Err("a sender name may only contain the letters a-z, digits and spaces".into());
    }
    if !(3..=11).contains(&n) || sender.trim() != sender {
        return Err(
            "a sender name must be 3 to 11 characters without leading or trailing spaces".into(),
        );
    }
    Ok(())
}

fn hex_colour(v: &str) -> bool {
    v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Image types a logo may have, with their media types.
pub fn logo_media_type(path: &str) -> Option<&'static str> {
    match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "svg" => Some("image/svg+xml"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

fn validate_branding(cfg: &Config, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
    let b = &cfg.branding;
    one_of(
        errors,
        "branding.theme",
        &b.theme,
        &["", "dark", "light", "auto"],
    );
    for (field, value) in [
        ("branding.accent_color", &b.accent_color),
        ("branding.logo_background", &b.logo_background),
    ] {
        if !value.is_empty() && !hex_colour(value) {
            errors.push(format!("{field}: must be a colour such as \"#e02424\""));
        }
    }
    for (field, value, max) in [
        ("branding.title", &b.title, 80),
        ("branding.subtitle", &b.subtitle, 120),
        ("branding.footer.company", &b.footer.company, 120),
        ("branding.footer.contact_name", &b.footer.contact_name, 120),
        (
            "branding.footer.contact_email",
            &b.footer.contact_email,
            200,
        ),
        ("branding.footer.text", &b.footer.text, 300),
    ] {
        if value.chars().count() > max || value.chars().any(char::is_control) {
            errors.push(format!(
                "{field}: is limited to {max} characters on one line"
            ));
        }
    }
    let mail = &b.footer.contact_email;
    if !mail.is_empty()
        && (!mail.contains('@')
            || mail
                .chars()
                .any(|c| c.is_whitespace() || "<>\"'".contains(c)))
    {
        errors.push("branding.footer.contact_email: is not an e-mail address".into());
    }
    if !b.footer.website.is_empty() && !valid_http_url(&b.footer.website) {
        errors.push("branding.footer.website: must be an http(s) URL".into());
    }
    if !b.logo.is_empty() {
        if !Path::new(&b.logo).is_absolute() {
            errors.push("branding.logo: must be an absolute path".into());
        } else if logo_media_type(&b.logo).is_none() {
            errors.push("branding.logo: must be a png, svg, jpg, webp or gif file".into());
        } else if !Path::new(&b.logo).is_file() {
            warnings.push(format!(
                "branding.logo: {} does not exist or is not readable; the built-in logo is shown",
                b.logo
            ));
        }
    }
}

fn validate_providers(cfg: &Config, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
    let mut names = HashSet::new();
    for p in &cfg.providers {
        let f = format!("provider[{}]", p.name);
        if p.name.is_empty()
            || !p
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
        {
            errors.push(format!(
                "{f}.name: must be non-empty and contain only letters, digits, '-' or '_'"
            ));
        }
        if !names.insert(p.name.clone()) {
            errors.push(format!("{f}.name: duplicate provider name"));
        }
        one_of(errors, &format!("{f}.type"), &p.kind, &["gatewayapi"]);
        one_of(errors, &format!("{f}.region"), &p.region, &["com", "eu"]);
        one_of(
            errors,
            &format!("{f}.priority"),
            &p.priority,
            &["normal", "urgent"],
        );
        positive(errors, &format!("{f}.timeout_seconds"), p.timeout_seconds);
        if !p.base_url.is_empty() && !valid_http_url(&p.base_url) {
            errors.push(format!("{f}.base_url: must be an http(s) URL"));
        }
        if p.display_name.chars().count() > 40 || p.display_name.chars().any(char::is_control) {
            errors.push(format!("{f}.display_name: is limited to 40 characters"));
        }
        if p.label.chars().count() > 128 {
            errors.push(format!("{f}.label: is limited to 128 characters"));
        }
        if !p.default_sender.is_empty() {
            if let Err(e) = valid_sender_name(&p.default_sender) {
                errors.push(format!("{f}.default_sender: {e}"));
            }
        }
        for a in p.allowed_senders.iter().filter(|a| a.as_str() != "*") {
            if let Err(e) = valid_sender_name(a) {
                errors.push(format!("{f}.allowed_senders: {a:?}: {e}"));
            }
        }
        if !p.enabled {
            continue;
        }
        if p.api_token.is_empty() {
            errors.push(format!(
                "{f}.api_token: is required for an enabled provider"
            ));
        }
        if p.default_sender.is_empty() && p.allowed_senders.is_empty() {
            errors.push(format!(
                "{f}: set default_sender or allowed_senders; without them nothing can be sent"
            ));
        }
        if p.allowed_senders.iter().any(|a| a == "*") {
            warnings.push(format!(
                "{f}.allowed_senders: \"*\" lets every API caller choose any sender name"
            ));
        }
        if p.callback_secret.is_empty() {
            warnings.push(format!(
                "{f}.callback_secret: not set; delivery status callbacks from the provider are rejected and messages stay `submitted`"
            ));
        }
        if p.daily_sms_limit == 0 && p.monthly_sms_limit == 0 {
            warnings.push(format!(
                "{f}: no daily_sms_limit or monthly_sms_limit; provider spending is unlimited"
            ));
        }
    }
}

fn validate(cfg: &Config, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
    validate_providers(cfg, errors, warnings);
    let g = &cfg.general;
    if g.instance_id.is_empty()
        || !g
            .instance_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        errors.push("general.instance_id: must be non-empty and contain only letters, digits, '-', '_' or '.'".into());
    }
    one_of(
        errors,
        "general.log_level",
        &g.log_level,
        &["trace", "debug", "info", "warn", "error"],
    );
    if g.timezone.parse::<chrono_tz::Tz>().is_err() {
        errors.push(format!("general.timezone: unknown time zone {:?} (use an IANA name such as \"UTC\" or \"Europe/Copenhagen\")", g.timezone));
    }
    for (name, p) in [
        ("paths.home", &cfg.paths.home),
        ("paths.data", &cfg.paths.data),
        ("paths.runtime", &cfg.paths.runtime),
        ("paths.recordings", &cfg.paths.recordings),
        ("paths.attachments", &cfg.paths.attachments),
        ("paths.backups", &cfg.paths.backups),
    ] {
        if !Path::new(p).is_absolute() {
            errors.push(format!("{name}: must be an absolute path"));
        }
    }

    let mut ids = HashSet::new();
    let mut devices = HashSet::new();
    for m in &cfg.modems {
        let f = format!("modem[{}]", m.id);
        if m.id.is_empty()
            || !m
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
        {
            errors.push(format!(
                "{f}.id: must contain only letters, digits, '-' or '_'"
            ));
        }
        if !ids.insert(m.id.clone()) {
            errors.push(format!("{f}.id: duplicate modem identifier"));
        }
        if m.enabled && !devices.insert(m.device.clone()) {
            errors.push(format!(
                "{f}.device: {} is used by more than one modem",
                m.device
            ));
        }
        if m.device.is_empty() {
            errors.push(format!("{f}.device: must not be empty"));
        }
        positive(errors, &format!("{f}.baud_rate"), m.baud_rate as u64);
        if !(5..=8).contains(&m.data_bits) {
            errors.push(format!("{f}.data_bits: must be between 5 and 8"));
        }
        if !(1..=2).contains(&m.stop_bits) {
            errors.push(format!("{f}.stop_bits: must be 1 or 2"));
        }
        one_of(
            errors,
            &format!("{f}.parity"),
            &m.parity,
            &["none", "even", "odd"],
        );
        one_of(
            errors,
            &format!("{f}.flow_control"),
            &m.flow_control,
            &["none", "hardware", "software"],
        );
        one_of(
            errors,
            &format!("{f}.preferred_network"),
            &m.preferred_network,
            &["auto", "2g", "3g", "4g"],
        );
        positive(
            errors,
            &format!("{f}.command_timeout_seconds"),
            m.command_timeout_seconds,
        );
        positive(
            errors,
            &format!("{f}.message_timeout_seconds"),
            m.message_timeout_seconds,
        );
        positive(
            errors,
            &format!("{f}.poll_interval_seconds"),
            m.poll_interval_seconds,
        );
        positive(
            errors,
            &format!("{f}.healthcheck_interval_seconds"),
            m.healthcheck_interval_seconds,
        );
        positive(
            errors,
            &format!("{f}.reconnect_delay_seconds"),
            m.reconnect_delay_seconds,
        );
        if m.max_reconnect_delay_seconds < m.reconnect_delay_seconds {
            errors.push(format!(
                "{f}.max_reconnect_delay_seconds: must not be smaller than reconnect_delay_seconds"
            ));
        }
        if !m.sim.is_empty() && cfg.sim(&m.sim).is_none() {
            errors.push(format!(
                "{f}.sim: SIM profile {:?} is not defined in {}",
                m.sim, cfg.files.sim
            ));
        }
        for c in &m.init_commands {
            if !c.to_ascii_uppercase().starts_with("AT") || c.contains(['\r', '\n']) {
                errors.push(format!(
                    "{f}.init_commands: {c:?} is not a single AT command"
                ));
            }
        }
    }
    if cfg.modems.len() > 1 && cfg.modems.iter().any(|m| m.sim.is_empty()) && cfg.sims.len() > 1 {
        warnings.push("several modems and SIM profiles are configured; set `sim` on each [[modems]] entry or fill in `iccid` in the SIM profiles so they can be matched".into());
    }

    let mut sim_ids = HashSet::new();
    for sp in &cfg.sims {
        let f = format!("sim[{}]", sp.id);
        if !sim_ids.insert(sp.id.clone()) {
            errors.push(format!("{f}.id: duplicate SIM identifier"));
        }
        if !sp.mobile_number.is_empty() && !valid_number(&sp.mobile_number) {
            errors.push(format!(
                "{f}.mobile_number: must be digits with an optional leading '+'"
            ));
        }
        if !sp.smsc.is_empty() && !valid_number(&sp.smsc) {
            errors.push(format!(
                "{f}.smsc: must be digits with an optional leading '+'"
            ));
        }
        if sp.pin_enabled
            && !(sp.pin.len() >= 4
                && sp.pin.len() <= 8
                && sp.pin.chars().all(|c| c.is_ascii_digit()))
        {
            errors.push(format!(
                "{f}.pin: must be 4 to 8 digits when pin_enabled is true"
            ));
        }
        if sp.pin_max_attempts == 0 || sp.pin_max_attempts > 2 {
            errors.push(format!(
                "{f}.pin_max_attempts: must be 1 or 2 (a third wrong PIN blocks the SIM)"
            ));
        }
        one_of(
            errors,
            &format!("{f}.network.selection"),
            &sp.network.selection,
            &["automatic", "manual"],
        );
        if sp.network.selection == "manual"
            && !(sp.network.operator_code.len() >= 5
                && sp.network.operator_code.chars().all(|c| c.is_ascii_digit()))
        {
            errors.push(format!("{f}.network.operator_code: a numeric MCC/MNC code is required for manual selection"));
        }
    }

    let sms = &cfg.sms;
    one_of(errors, "sms.mode", &sms.mode, &["pdu"]);
    one_of(
        errors,
        "sms.default_encoding",
        &sms.default_encoding,
        &["gsm7", "ucs2"],
    );
    if sms.max_message_parts == 0 || sms.max_message_parts > 255 {
        errors.push("sms.max_message_parts: must be between 1 and 255".into());
    }
    positive(
        errors,
        "sms.max_send_attempts",
        sms.max_send_attempts as u64,
    );
    positive(
        errors,
        "sms.multipart_timeout_seconds",
        sms.multipart_timeout_seconds,
    );
    if !sms.default_modem.is_empty() && cfg.modem(&sms.default_modem).is_none() {
        errors.push(format!(
            "sms.default_modem: modem {:?} is not configured",
            sms.default_modem
        ));
    }

    if cfg.mms.enabled && cfg.mms.download_attachments && !valid_http_url(&cfg.mms.mmsc_url) {
        errors.push("mms.mmsc_url: a valid http(s) MMSC URL is required when download_attachments is enabled".into());
    }
    if !cfg.mms.proxy.is_empty() && !valid_http_url(&cfg.mms.proxy) {
        errors.push("mms.proxy: must be an http(s) URL".into());
    }
    let ussd_re = Regex::new(r"^[*#][0-9*#]{1,40}#$").expect("static regex");
    for (name, code) in &cfg.ussd.commands {
        if !ussd_re.is_match(code) {
            errors.push(format!(
                "ussd.commands.{name}: {code:?} is not a valid USSD code"
            ));
        }
    }
    positive(errors, "ussd.timeout_seconds", cfg.ussd.timeout_seconds);

    one_of(
        errors,
        "database.driver",
        &cfg.database.driver,
        &["sqlite", "mysql"],
    );
    match cfg.database.driver.as_str() {
        "mysql" if !cfg.database.mysql_url.starts_with("mysql://") => errors.push(
            "database.mysql_url: must be set to a mysql:// URL when driver is \"mysql\"".into(),
        ),
        "sqlite" if cfg.database.sqlite_path.is_empty() => {
            errors.push("database.sqlite_path: must not be empty".into())
        }
        _ => {}
    }
    positive(
        errors,
        "database.max_connections",
        cfg.database.max_connections as u64,
    );
    positive(
        errors,
        "retention.cleanup_interval_seconds",
        cfg.retention.cleanup_interval_seconds,
    );

    let api = &cfg.api;
    if api.bind.parse::<std::net::IpAddr>().is_err() {
        errors.push(format!(
            "api.bind: {:?} is not a valid IP address",
            api.bind
        ));
    }
    positive(errors, "api.port", api.port as u64);
    one_of(
        errors,
        "api.authentication",
        &api.authentication,
        &["none", "bearer"],
    );
    if api.authentication == "bearer"
        && api.tokens.iter().all(|t| t.is_empty())
        && api.admin_tokens.iter().all(|t| t.is_empty())
    {
        errors.push(
            "api.tokens: at least one token in api.tokens or api.admin_tokens is required when authentication is \"bearer\" (managed tokens are created from the dashboard)".into(),
        );
    }
    for t in api.tokens.iter().chain(&api.admin_tokens) {
        if !t.is_empty() && t.len() < 16 {
            errors.push("api.tokens: tokens must be at least 16 characters long".into());
            break;
        }
    }
    validate_cidrs(errors, "api.allowed_cidrs", &api.allowed_cidrs);
    validate_cidrs(errors, "api.admin_allowed_cidrs", &api.admin_allowed_cidrs);
    validate_cidrs(errors, "api.trusted_proxies", &api.trusted_proxies);
    positive(
        errors,
        "api.max_request_size_kb",
        api.max_request_size_kb as u64,
    );
    positive(
        errors,
        "api.request_timeout_seconds",
        api.request_timeout_seconds,
    );
    if api.cors_enabled && api.cors_allowed_origins.is_empty() {
        errors.push(
            "api.cors_allowed_origins: at least one origin is required when cors_enabled is true"
                .into(),
        );
    }
    if api.enabled
        && api.authentication == "none"
        && api
            .allowed_cidrs
            .iter()
            .any(|c| c == "0.0.0.0/0" || c == "::/0")
    {
        warnings.push("api: authentication is disabled and allowed_cidrs admits every address; never expose this API to the public internet".into());
    }
    one_of(
        errors,
        "web.theme",
        &cfg.web.theme,
        &["dark", "light", "auto"],
    );
    validate_branding(cfg, errors, warnings);
    positive(
        errors,
        "web.refresh_interval_seconds",
        cfg.web.refresh_interval_seconds,
    );

    one_of(
        errors,
        "logging.format",
        &cfg.logging.format,
        &["json", "text"],
    );
    one_of(
        errors,
        "logging.rotation",
        &cfg.logging.rotation,
        &["daily", "hourly", "never"],
    );
    one_of(
        errors,
        "calls.audio_backend",
        &cfg.calls.audio_backend,
        &["auto", "alsa", "none"],
    );
    one_of(
        errors,
        "calls.recording_format",
        &cfg.calls.recording_format,
        &["wav"],
    );
    if cfg.calls.recording_duration_seconds == 0 || cfg.calls.recording_duration_seconds > 300 {
        errors.push("calls.recording_duration_seconds: must be between 1 and 300".into());
    }
    if cfg.calls.recording_enabled && !cfg.calls.auto_answer {
        warnings.push(
            "calls.recording_enabled has no effect unless calls.auto_answer is also enabled".into(),
        );
    }
    positive(
        errors,
        "gnss.poll_interval_seconds",
        cfg.gnss.poll_interval_seconds,
    );
    if !cfg.gnss.modem.is_empty() && cfg.modem(&cfg.gnss.modem).is_none() {
        errors.push(format!(
            "gnss.modem: modem {:?} is not configured",
            cfg.gnss.modem
        ));
    }
    if cfg.integrations.worker_count == 0 || cfg.integrations.worker_count > 64 {
        errors.push("integrations.worker_count: must be between 1 and 64".into());
    }
    positive(
        errors,
        "integrations.max_attempts",
        cfg.integrations.max_attempts as u64,
    );
    positive(
        errors,
        "integrations.retry_initial_seconds",
        cfg.integrations.retry_initial_seconds,
    );
    if !cfg.metrics.path.starts_with('/') {
        errors.push("metrics.path: must start with '/'".into());
    }
    positive(
        errors,
        "security.max_outbound_queue_size",
        cfg.security.max_outbound_queue_size,
    );

    if cfg.anonymous.label.trim().is_empty() {
        errors.push("anonymous.label: must not be empty".into());
    }
    for n in &cfg.anonymous.numbers {
        let base = n.trim_end_matches('*');
        if base.is_empty() || base.chars().any(|c| c.is_control()) {
            errors.push(format!(
                "anonymous.numbers: {n:?} is not a number or prefix"
            ));
        }
    }

    let mut names = HashSet::new();
    for (i, d) in cfg.integration.iter().enumerate() {
        let f = if d.name.is_empty() {
            format!("integration[{i}]")
        } else {
            format!("integration[{}]", d.name)
        };
        if d.name.is_empty()
            || !d
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
        {
            errors.push(format!(
                "{f}.name: must be non-empty and contain only letters, digits, '-' or '_'"
            ));
        }
        if !names.insert(d.name.clone()) {
            errors.push(format!("{f}.name: duplicate integration name"));
        }
        one_of(errors, &format!("{f}.type"), &d.kind, INTEGRATION_TYPES);
        validate_events(errors, &format!("{f}.events"), &d.events);
        validate_filter(errors, &format!("{f}.filter"), &d.filter);
        positive(errors, &format!("{f}.timeout_seconds"), d.timeout_seconds);
        if !d.enabled {
            continue;
        }
        if d.events.is_empty() {
            warnings.push(format!("{f}: enabled but subscribed to no events"));
        }
        let need_url = |errors: &mut Vec<String>, key: &str, v: &str| {
            if !valid_http_url(v) {
                errors.push(format!("{f}.{key}: a valid http(s) URL is required"));
            }
        };
        match d.kind.as_str() {
            "webhook" => {
                need_url(errors, "url", &d.url);
                one_of(
                    errors,
                    &format!("{f}.method"),
                    &d.method.to_ascii_uppercase(),
                    &["GET", "POST", "PUT"],
                );
                if d.sign_payload && d.signing_secret.is_empty() {
                    errors.push(format!(
                        "{f}.signing_secret: required when sign_payload is true"
                    ));
                }
                if d.method.eq_ignore_ascii_case("GET") {
                    warnings.push(format!("{f}: GET webhooks place message data in the URL, where proxies and access logs may record it"));
                }
            }
            "discord" | "slack" | "teams" => need_url(errors, "webhook_url", &d.webhook_url),
            "telegram" => {
                if d.bot_token.is_empty() {
                    errors.push(format!("{f}.bot_token: required"));
                }
                if d.chat_id.is_empty() {
                    errors.push(format!("{f}.chat_id: required"));
                }
            }
            "ntfy" => {
                need_url(errors, "url", &d.url);
                if d.topic.is_empty() {
                    errors.push(format!("{f}.topic: required"));
                }
            }
            "gotify" => {
                need_url(errors, "url", &d.url);
                if d.token.is_empty() {
                    errors.push(format!("{f}.token: required"));
                }
            }
            "syslog"
                if d.server
                    .rsplit_once(':')
                    .and_then(|(_, p)| p.parse::<u16>().ok())
                    .is_none() =>
            {
                errors.push(format!("{f}.server: must be host:port"));
            }
            _ => {}
        }
        if !d.verify_tls {
            warnings.push(format!("{f}: TLS certificate verification is disabled"));
        }
    }
    let mut rule_names = HashSet::new();
    for (i, r) in cfg.routing.iter().enumerate() {
        let f = if r.name.is_empty() {
            format!("routing.rule[{i}]")
        } else {
            format!("routing.rule[{}]", r.name)
        };
        if r.name.is_empty() || !rule_names.insert(r.name.clone()) {
            errors.push(format!("{f}.name: must be non-empty and unique"));
        }
        validate_events(errors, &format!("{f}.events"), &r.events);
        validate_filter(errors, &f, &r.filter);
        for d in &r.destinations {
            if cfg.integration_def(d).is_none() {
                errors.push(format!(
                    "{f}.destinations: integration {d:?} is not defined"
                ));
            }
        }
    }
    let mut auto_names = HashSet::new();
    for (i, r) in cfg.automation.iter().enumerate() {
        let f = if r.name.is_empty() {
            format!("automation.rule[{i}]")
        } else {
            format!("automation.rule[{}]", r.name)
        };
        if r.name.is_empty() || !auto_names.insert(r.name.clone()) {
            errors.push(format!("{f}.name: must be non-empty and unique"));
        }
        one_of(
            errors,
            &format!("{f}.action"),
            &r.action,
            &["reply", "forward"],
        );
        validate_filter(errors, &f, &r.filter);
        if !r.modem.is_empty() && cfg.modem(&r.modem).is_none() {
            errors.push(format!("{f}.modem: modem {:?} is not configured", r.modem));
        }
        if !r.enabled {
            continue;
        }
        if r.template.is_empty() {
            errors.push(format!("{f}.template: required"));
        }
        if r.max_per_hour == 0 {
            errors.push(format!("{f}.max_per_hour: must be greater than zero"));
        }
        if r.action == "forward" {
            if r.to.is_empty() {
                errors.push(format!(
                    "{f}.to: at least one destination is required for forwarding"
                ));
            }
            for n in &r.to {
                if !valid_number(n) {
                    errors.push(format!("{f}.to: {n:?} is not a valid telephone number"));
                }
            }
        }
        if r.action == "reply"
            && r.filter.sender_allowlist.is_empty()
            && r.filter.sender_regex.is_empty()
        {
            errors.push(format!("{f}: automatic replies require sender_allowlist or sender_regex so that they never answer arbitrary senders"));
        }
    }
}

const SECRET_KEYS: &[&str] = &[
    "pin",
    "password",
    "mysql_url",
    "tokens",
    "admin_tokens",
    "signing_secret",
    "bot_token",
    "token",
    "api_token",
    "callback_secret",
    "webhook_url",
    "chat_id",
];

fn mask_tail(s: &str) -> String {
    let n = s.chars().count();
    if n <= 4 {
        "*".repeat(n)
    } else {
        format!(
            "{}{}",
            "*".repeat(n - 4),
            s.chars().skip(n - 4).collect::<String>()
        )
    }
}

fn redact_value(key: &str, v: &mut Json) {
    match v {
        Json::Object(o) => {
            for (k, child) in o.iter_mut() {
                if k == "headers" {
                    if let Json::Object(h) = child {
                        for (_, hv) in h.iter_mut() {
                            *hv = Json::String("***".into());
                        }
                    }
                    continue;
                }
                redact_value(k, child);
            }
        }
        Json::Array(a) => {
            for child in a.iter_mut() {
                redact_value(key, child);
            }
        }
        Json::String(s) if !s.is_empty() => {
            if SECRET_KEYS.contains(&key) {
                *s = "***".into();
            } else if key == "iccid" || key == "imsi" {
                *s = mask_tail(s);
            } else if key == "url" || key == "proxy" || key == "mmsc_url" {
                if let Ok(u) = reqwest::Url::parse(s) {
                    let secretive =
                        !u.username().is_empty() || u.password().is_some() || u.query().is_some();
                    if key == "url" || secretive {
                        *s = format!(
                            "{}://{}{}/***",
                            u.scheme(),
                            u.host_str().unwrap_or(""),
                            u.port().map(|p| format!(":{p}")).unwrap_or_default()
                        );
                    }
                }
            }
        }
        _ => {}
    }
}

impl Config {
    /// JSON view of the configuration with every secret masked.
    pub fn redacted(&self) -> Json {
        let mut v = serde_json::to_value(self).unwrap_or(Json::Null);
        redact_value("", &mut v);
        v
    }

    /// Settings that differ from `new` and only take effect after a restart.
    pub fn restart_required(&self, new: &Config) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.database != new.database {
            out.push("database");
        }
        if (self.api.enabled, &self.api.bind, self.api.port)
            != (new.api.enabled, &new.api.bind, new.api.port)
        {
            out.push("api.enabled/bind/port");
        }
        let l = |c: &Config| {
            let l = &c.logging;
            (
                l.system_log.clone(),
                l.messages_log.clone(),
                l.integrations_log.clone(),
                l.audit_log.clone(),
                l.format.clone(),
                l.journal_enabled,
                l.rotation.clone(),
                l.retention_days,
            )
        };
        if l(self) != l(new) {
            out.push("logging (files, format, rotation)");
        }
        if self.paths != new.paths {
            out.push("paths");
        }
        // Enabling or disabling a configured modem is applied live; only a
        // changed set of modems needs a restart.
        let ids = |c: &Config| c.modems.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
        if ids(self) != ids(new) {
            out.push("modems (adding or removing a modem)");
        }
        if (
            self.api.max_request_size_kb,
            self.api.request_timeout_seconds,
        ) != (new.api.max_request_size_kb, new.api.request_timeout_seconds)
        {
            out.push("api.max_request_size_kb/request_timeout_seconds");
        }
        if self.web.enabled != new.web.enabled {
            out.push("web.enabled");
        }
        if self.metrics != new.metrics {
            out.push("metrics");
        }
        if self.integrations.persistent_queue != new.integrations.persistent_queue {
            out.push("integrations.persistent_queue");
        }
        if self.integrations.worker_count != new.integrations.worker_count {
            out.push("integrations.worker_count");
        }
        if self.general.instance_id != new.general.instance_id {
            out.push("general.instance_id");
        }
        out
    }

    pub fn timezone(&self) -> chrono_tz::Tz {
        self.general.timezone.parse().unwrap_or(chrono_tz::UTC)
    }
}

pub fn shared(cfg: Config) -> SharedConfig {
    Arc::new(ArcSwap::from_pointee(cfg))
}

#[cfg(test)]
mod tests;
