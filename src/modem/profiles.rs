//! Modem profiles: declarative, vendor-specific behaviour loaded from TOML.
//! Built-in profiles are embedded; files in the configured profile directory
//! override or extend them without recompiling.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GnssProfile {
    /// simcom_cgns, simcom_cgps, quectel_qgps or empty for none
    pub kind: String,
    pub test_command: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModemProfile {
    pub name: String,
    pub description: String,
    /// Case-insensitive substrings of the manufacturer identification.
    pub manufacturers: Vec<String>,
    /// Case-insensitive substrings of the model identification.
    pub models: Vec<String>,
    pub init_commands: Vec<String>,
    pub cnmi: Vec<String>,
    pub iccid_commands: Vec<String>,
    pub network_technologies: Vec<String>,
    /// supported, unsupported, unverified or unknown
    pub voice: String,
    pub audio_interfaces: Vec<String>,
    pub mms_test_command: String,
    pub bluetooth_test_command: String,
    /// Preferred SMS storage, e.g. "SM"; empty keeps the modem default.
    pub sms_storage: String,
    /// Shown when the modem does not answer.
    pub power_on_hint: String,
    pub gnss: GnssProfile,
    pub network_modes: BTreeMap<String, String>,
}

impl Default for ModemProfile {
    fn default() -> Self {
        Self {
            name: "generic".into(),
            description: String::new(),
            manufacturers: vec![],
            models: vec![],
            init_commands: vec![],
            cnmi: vec!["2,1,0,1,0".into(), "2,1,0,0,0".into(), "1,1,0,0,0".into()],
            iccid_commands: vec!["AT+CCID".into()],
            network_technologies: vec![],
            voice: "unknown".into(),
            audio_interfaces: vec![],
            mms_test_command: String::new(),
            bluetooth_test_command: String::new(),
            sms_storage: String::new(),
            power_on_hint: String::new(),
            gnss: GnssProfile::default(),
            network_modes: BTreeMap::new(),
        }
    }
}

const BUILTIN: &[(&str, &str)] = &[
    (
        "generic",
        include_str!("../../config/modem-profiles/generic.toml"),
    ),
    (
        "simcom-2g",
        include_str!("../../config/modem-profiles/simcom-2g.toml"),
    ),
    (
        "simcom-lte",
        include_str!("../../config/modem-profiles/simcom-lte.toml"),
    ),
    (
        "quectel",
        include_str!("../../config/modem-profiles/quectel.toml"),
    ),
];

#[derive(Debug, Clone)]
pub struct ProfileSet {
    profiles: Vec<ModemProfile>,
}

impl ProfileSet {
    /// Built-in profiles plus `*.toml` files from `dir` (same name overrides).
    /// Returns the set and warnings for files that could not be loaded.
    pub fn load(dir: &str) -> (Self, Vec<String>) {
        let mut profiles: Vec<ModemProfile> = BUILTIN
            .iter()
            .map(|(name, text)| {
                toml::from_str(text)
                    .unwrap_or_else(|e| panic!("built-in profile {name} is invalid: {e}"))
            })
            .collect();
        let mut warnings = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut files: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
                .collect();
            files.sort();
            for path in files {
                let parsed = std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| toml::from_str::<ModemProfile>(&t).map_err(|e| e.to_string()));
                match parsed {
                    Ok(p) if !p.name.is_empty() => {
                        profiles.retain(|x| x.name != p.name);
                        profiles.push(p);
                    }
                    Ok(_) => warnings.push(format!("{}: profile has no name", path.display())),
                    Err(e) => warnings.push(format!("{}: {}", path.display(), e.trim_end())),
                }
            }
        }
        (Self { profiles }, warnings)
    }

    pub fn names(&self) -> Vec<String> {
        self.profiles.iter().map(|p| p.name.clone()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&ModemProfile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Pick the profile for an identified modem. Model matches win over
    /// manufacturer-only matches; `generic` is the fallback.
    pub fn detect(&self, manufacturer: &str, model: &str) -> &ModemProfile {
        let (man, model) = (manufacturer.to_uppercase(), model.to_uppercase());
        let hit = |list: &[String], hay: &str| list.iter().any(|m| hay.contains(&m.to_uppercase()));
        self.profiles
            .iter()
            .find(|p| !p.models.is_empty() && hit(&p.models, &model))
            .or_else(|| {
                self.profiles.iter().find(|p| {
                    p.models.is_empty()
                        && !p.manufacturers.is_empty()
                        && hit(&p.manufacturers, &man)
                })
            })
            .or_else(|| self.get("generic"))
            .unwrap_or(&self.profiles[0])
    }

    /// Resolve the configured profile name (`auto` detects).
    pub fn resolve(&self, configured: &str, manufacturer: &str, model: &str) -> ModemProfile {
        match configured {
            "" | "auto" => self.detect(manufacturer, model).clone(),
            name => self
                .get(name)
                .cloned()
                .unwrap_or_else(|| self.detect(manufacturer, model).clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_profiles_parse_and_detect() {
        let (set, warnings) = ProfileSet::load("/nonexistent");
        assert!(warnings.is_empty());
        assert_eq!(set.detect("SIMCOM_Ltd", "SIMCOM_SIM868").name, "simcom-2g");
        assert_eq!(
            set.detect("SIMCOM INCORPORATED", "SIMCOM_SIM7600E-H").name,
            "simcom-lte"
        );
        assert_eq!(set.detect("Quectel", "EC25").name, "quectel");
        assert_eq!(set.detect("Acme", "X1").name, "generic");
        assert_eq!(set.resolve("quectel", "SIMCOM", "SIM800").name, "quectel");
        assert_eq!(
            set.resolve("missing", "SIMCOM", "SIM800L").name,
            "simcom-2g"
        );
        assert_eq!(set.get("simcom-2g").unwrap().gnss.kind, "simcom_cgns");
        assert!(set.get("quectel").unwrap().network_modes.contains_key("4g"));
    }

    #[test]
    fn directory_profiles_override() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("custom.toml"),
            "name = \"acme\"\nmodels = [\"X1\"]\nvoice = \"unsupported\"",
        )
        .unwrap();
        std::fs::write(
            d.path().join("simcom-2g.toml"),
            "name = \"simcom-2g\"\nmodels = [\"SIM800\"]\nsms_storage = \"ME\"",
        )
        .unwrap();
        std::fs::write(d.path().join("broken.toml"), "name = [").unwrap();
        let (set, warnings) = ProfileSet::load(d.path().to_str().unwrap());
        assert_eq!(warnings.len(), 1);
        assert_eq!(set.detect("Acme", "X1").name, "acme");
        assert_eq!(set.get("simcom-2g").unwrap().sms_storage, "ME");
    }
}
