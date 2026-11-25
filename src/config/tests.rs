use super::*;
use std::fs;

struct Dir(tempfile::TempDir);

impl Dir {
    fn new(main: &str, sim: &str, integrations: &str) -> Self {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        let main = format!(
            "{main}\n[paths]\nsim_config = \"{}\"\naddressbook = \"{}\"\nanonymous = \"{}\"\nproviders = \"{}\"\nbranding = \"{}\"\n[integrations]\nconfig_file = \"{}\"\n",
            p.join("sim.conf").display(),
            p.join("addressbook.conf").display(),
            p.join("anonymous.conf").display(),
            p.join("providers.conf").display(),
            p.join("branding.conf").display(),
            p.join("integrations.conf").display()
        );
        fs::write(p.join("smsgateway.conf"), main).unwrap();
        fs::write(p.join("sim.conf"), sim).unwrap();
        fs::write(p.join("integrations.conf"), integrations).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p.join("sim.conf"), fs::Permissions::from_mode(0o600)).unwrap();
        Dir(d)
    }
    fn opts(&self) -> LoadOptions {
        let mut o = LoadOptions::new(self.0.path().join("smsgateway.conf"));
        o.use_env = false;
        o
    }
    fn load(&self) -> Result<Config, ConfigError> {
        load(&self.opts())
    }
}

#[test]
fn defaults_are_valid() {
    let cfg = Dir::new("", "", "").load().unwrap();
    assert_eq!(cfg.api.port, 8080);
    assert_eq!(cfg.modems.len(), 1);
    assert_eq!(cfg.modems[0].device, "/dev/ttyS0");
    assert_eq!(cfg.modems[0].id, "modem-1");
    assert_eq!(cfg.sims.len(), 1);
    assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
}

#[test]
fn field_errors_name_the_field() {
    let err = Dir::new(
        "[api]\nport = 0\nbind = \"nope\"\nallowed_cidrs = [\"300.1.1.1/8\"]\n[modem]\nparity = \"sideways\"\n[sms]\nmode = \"text\"\n[general]\ntimezone = \"Mars/Olympus\"",
        "",
        "",
    )
    .load()
    .unwrap_err();
    let all = err.to_string();
    for needle in [
        "api.port",
        "api.bind",
        "api.allowed_cidrs",
        "modem[modem-1].parity",
        "sms.mode",
        "general.timezone",
    ] {
        assert!(all.contains(needle), "missing {needle} in {all}");
    }
}

#[test]
fn type_errors_are_reported() {
    let err = Dir::new("[api]\nport = \"eighty\"", "", "")
        .load()
        .unwrap_err();
    assert!(err.to_string().contains("[api]"), "{err}");
}

#[test]
fn unknown_keys_warn() {
    let cfg = Dir::new(
        "[api]\nprot = 1\n[bogus]\nx = 1",
        "[sim]\nnmae = \"x\"",
        "[[integration]]\nname = \"a\"\ntype = \"webhook\"\ncolour = \"red\"\n[[routing.rule]]\nname = \"r\"\nsender_allowlist = [\"x\"]\nwhat = 1",
    )
    .load()
    .unwrap();
    let w = cfg.warnings.join("\n");
    for needle in [
        "api.prot",
        "bogus",
        "sim.nmae",
        "integration[0].colour",
        "routing.rule[0].what",
    ] {
        assert!(w.contains(needle), "missing {needle} in {w}");
    }
    assert!(!w.contains("sender_allowlist"));
}

#[test]
fn multi_modem_layering() {
    let cfg = Dir::new(
        "[modem]\nbaud_rate = 9600\ndevice = \"/dev/ttyUSB0\"\n[[modems]]\nid = \"a\"\nsim = \"s1\"\n[[modems]]\ndevice = \"/dev/ttyUSB1\"\nbaud_rate = 115200\nsim = \"s2\"",
        "[[sims]]\nid = \"s1\"\nmobile_number = \"+4500000001\"\n[sims.limits]\ndaily_sms_limit = 5\n[[sims]]\nid = \"s2\"\nmobile_number = \"+4500000002\"",
        "",
    )
    .load()
    .unwrap();
    assert_eq!(cfg.modems.len(), 2);
    assert_eq!(
        (cfg.modems[0].id.as_str(), cfg.modems[0].baud_rate),
        ("a", 9600)
    );
    assert_eq!(
        (cfg.modems[1].id.as_str(), cfg.modems[1].baud_rate),
        ("modem-2", 115200)
    );
    assert_eq!(cfg.sim_for_modem("a").unwrap().limits.daily_sms_limit, 5);
    assert_eq!(
        cfg.sim_for_modem("modem-2").unwrap().mobile_number,
        "+4500000002"
    );
}

#[test]
fn duplicate_devices_and_unknown_sim_rejected() {
    let err = Dir::new(
        "[[modems]]\nid = \"a\"\nsim = \"nope\"\n[[modems]]\nid = \"b\"",
        "",
        "",
    )
    .load()
    .unwrap_err()
    .to_string();
    assert!(err.contains("modem[b].device"), "{err}");
    assert!(err.contains("modem[a].sim"), "{err}");
}

#[test]
fn classic_single_sim_layout() {
    let cfg = Dir::new(
        "",
        "[sim]\nname = \"Primary\"\nmobile_number = \"+4500000000\"\n[network]\napn = \"internet\"\n[limits]\ndaily_sms_limit = 7",
        "",
    )
    .load()
    .unwrap();
    let s = cfg.sim_for_modem("modem-1").unwrap();
    assert_eq!(
        (
            s.name.as_str(),
            s.network.apn.as_str(),
            s.limits.daily_sms_limit
        ),
        ("Primary", "internet", 7)
    );
}

#[test]
fn overrides_and_secret_references() {
    let d = Dir::new("", "", "");
    let secret = d.0.path().join("pin");
    fs::write(&secret, "4321\n").unwrap();
    fs::write(
        d.0.path().join("sim.conf"),
        format!(
            "[sim]\npin_enabled = true\npin = \"${{file:{}}}\"",
            secret.display()
        ),
    )
    .unwrap();
    let mut o = d.opts();
    o.overrides = vec![
        "api.port=9099".into(),
        "modem.device=/dev/ttyUSB7".into(),
        "sms.allow_unicode=false".into(),
    ];
    let cfg = load(&o).unwrap();
    assert_eq!(cfg.api.port, 9099);
    assert_eq!(cfg.modems[0].device, "/dev/ttyUSB7");
    assert!(!cfg.sms.allow_unicode);
    assert_eq!(cfg.sims[0].pin, "4321");
}

#[test]
fn missing_secret_fails_only_when_enabled() {
    let body = |enabled: bool| {
        format!("[[integration]]\nname = \"d\"\ntype = \"discord\"\nenabled = {enabled}\nevents = [\"sms.received\"]\nwebhook_url = \"${{THUGSRED_TEST_UNSET_VARIABLE}}\"")
    };
    assert!(Dir::new("", "", &body(false)).load().is_ok());
    let err = Dir::new("", "", &body(true))
        .load()
        .unwrap_err()
        .to_string();
    assert!(err.contains("THUGSRED_TEST_UNSET_VARIABLE"), "{err}");
}

#[test]
fn redaction_hides_every_secret() {
    let cfg = Dir::new(
        "[api]\nauthentication = \"bearer\"\ntokens = [\"super-secret-token-value\"]\n[database]\ndriver = \"mysql\"\nmysql_url = \"mysql://user:hunter2pass@db.example.invalid/sms\"",
        "[sim]\npin = \"9876\"\npin_enabled = true\niccid = \"8945000000000012345\"\n[network]\npassword = \"apn-pass-word\"",
        "[[integration]]\nname = \"t\"\ntype = \"telegram\"\nenabled = true\nevents = [\"sms.received\"]\nbot_token = \"123456:ABCDEF-secret\"\nchat_id = \"555\"\n[[integration]]\nname = \"w\"\ntype = \"webhook\"\nenabled = true\nevents = [\"*\"]\nurl = \"https://example.invalid/hook/secret-path?key=abc\"\nsign_payload = true\nsigning_secret = \"hmac-secret-value\"\n[integration.headers]\nAuthorization = \"Bearer header-secret\"",
    )
    .load()
    .unwrap();
    let out = serde_json::to_string(&cfg.redacted()).unwrap();
    for secret in [
        "super-secret-token-value",
        "hunter2pass",
        "9876",
        "apn-pass-word",
        "ABCDEF-secret",
        "secret-path",
        "hmac-secret-value",
        "header-secret",
        "8945000000000012345",
    ] {
        assert!(!out.contains(secret), "{secret} leaked");
    }
    assert!(out.contains("example.invalid"));
    assert!(out.contains("***************2345"));
}

#[test]
fn routing_and_automation_validation() {
    let err = Dir::new(
        "",
        "",
        "[[routing.rule]]\nname = \"r\"\nevents = [\"sms.nope\"]\nmessage_regex = \"(\"\ndestinations = [\"ghost\"]\n[[automation.rule]]\nname = \"a\"\nenabled = true\naction = \"reply\"\ntemplate = \"hi\"",
    )
    .load()
    .unwrap_err()
    .to_string();
    for needle in [
        "unknown event",
        "message_regex",
        "\"ghost\"",
        "automatic replies require",
    ] {
        assert!(err.contains(needle), "missing {needle} in {err}");
    }
}

#[test]
fn event_patterns() {
    assert!(event_matches("*", "sms.received"));
    assert!(event_matches("sms.*", "sms.received"));
    assert!(!event_matches("sms.*", "smsx.received"));
    assert!(!event_matches("sms.sent", "sms.received"));
}

#[test]
fn restart_required_detection() {
    let a = Dir::new("", "", "").load().unwrap();
    let mut b = a.clone();
    b.api.port = 9000;
    b.sms.outbound_rate_limit_per_minute = 99;
    assert_eq!(a.restart_required(&b), vec!["api.enabled/bind/port"]);
}

#[test]
fn addressbook_and_anonymous_list() {
    let d = Dir::new("", "", "[[integration]]\nname = \"d1\"\ntype = \"discord\"\nanonymize = true\ninclude_message_body = false\n[integration.filter]\nsender_allowlist = [\"+4500000001\"]\n[[integration]]\nname = \"d2\"\ntype = \"discord\"");
    let p = d.0.path();
    fs::write(p.join("addressbook.conf"), "[contacts]\n\"+45 00 00 00 01\" = \"Alice\"\n[[contact]]\nname = \"Bob\"\nnumber = \"+4512345672\"\n").unwrap();
    fs::write(
        p.join("anonymous.conf"),
        "numbers = [\"+4500000009\", \"+4599*\"]\nlabel = \"Hidden\"\n",
    )
    .unwrap();
    let mut o = d.opts();
    o.overrides = vec![
        format!("paths.addressbook={}", p.join("addressbook.conf").display()),
        format!("paths.anonymous={}", p.join("anonymous.conf").display()),
    ];
    let cfg = load(&o).unwrap();
    assert_eq!(cfg.contact_name("+4500000001"), Some("Alice"));
    assert_eq!(
        cfg.contact_name("12345672"),
        Some("Bob"),
        "national format matches"
    );
    assert_eq!(cfg.contact_name("+4500000003"), None);
    assert!(
        cfg.is_anonymous("+4500000009")
            && cfg.is_anonymous("+4599123456")
            && cfg.is_anonymous("0045 00 00 00 09")
    );
    assert!(!cfg.is_anonymous("+4500000001") && !cfg.is_anonymous(""));
    assert_eq!(cfg.anonymous.label, "Hidden");
    assert!(cfg.integration[0].anonymize && !cfg.integration[0].include_message_body);
    assert_eq!(
        cfg.integration[0].filter.sender_allowlist,
        vec!["+4500000001"]
    );
    assert_eq!(
        cfg.integration.len(),
        2,
        "several integrations of one type are allowed"
    );
    // a broken address book is rejected with the file name
    fs::write(
        p.join("addressbook.conf"),
        "[[contact]]\nname = \"NoNumber\"\n",
    )
    .unwrap();
    assert!(load(&o)
        .unwrap_err()
        .to_string()
        .contains("every contact needs"));
}

#[test]
fn env_file_next_to_config_resolves_references() {
    let d = Dir::new(
        "[database]\ndriver = \"mysql\"\nmysql_url = \"${THUGSRED_TEST_ENVFILE_URL}\"",
        "",
        "",
    );
    assert!(d
        .load()
        .unwrap_err()
        .to_string()
        .contains("THUGSRED_TEST_ENVFILE_URL"));
    fs::write(d.0.path().join("smsgateway.env"), "# secrets\nTHUGSRED_TEST_ENVFILE_URL=\"mysql://u:secretpw123@db.example.invalid/sms\"\nOTHER = x\n").unwrap();
    let cfg = d.load().unwrap();
    assert_eq!(
        cfg.database.mysql_url,
        "mysql://u:secretpw123@db.example.invalid/sms"
    );
    assert!(!serde_json::to_string(&cfg.redacted())
        .unwrap()
        .contains("secretpw123"));
}

#[test]
fn providers_file() {
    let d = Dir::new("", "", "");
    let p = d.0.path().join("providers.conf");
    let write = |text: &str| {
        fs::write(&p, text).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    };
    write("[[provider]]\nname = \"gw\"\nenabled = true\napi_token = \"secret-token-value\"\ndefault_sender = \"Example\"\ncallback_secret = \"cb\"\ndaily_sms_limit = 5\n");
    let cfg = d.load().unwrap();
    assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
    assert_eq!(cfg.provider("gw").unwrap().region, "com");
    let shown = cfg.redacted().to_string();
    assert!(!shown.contains("secret-token-value") && !shown.contains("\"cb\""));

    // A disabled provider needs nothing; an enabled one needs a token and a sender.
    write("[[provider]]\nname = \"gw\"\n");
    assert!(d.load().unwrap().provider("gw").is_none());
    write("[[provider]]\nname = \"gw\"\nenabled = true\nregion = \"mars\"\ndefault_sender = \"way too long a name\"\n[[provider]]\nname = \"gw\"\n");
    let err = d.load().unwrap_err().to_string();
    for needle in [
        "api_token",
        "region",
        "default_sender",
        "duplicate provider name",
    ] {
        assert!(err.contains(needle), "{needle} missing in {err}");
    }
    assert!(valid_sender_name("Example 1").is_ok());
    assert!(valid_sender_name("4500000000").is_ok());
    for bad in ["ab", "Twelve chars", "Bad!", " pad", "1234567890123456", ""] {
        assert!(valid_sender_name(bad).is_err(), "{bad}");
    }
}
