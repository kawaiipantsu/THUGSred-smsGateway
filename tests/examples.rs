//! The shipped example configuration must load cleanly: every documented
//! option has to exist, and defaults in the example must match the code.

use thugsred_smsgateway::config::{self, Config, LoadOptions};

fn load_examples() -> Config {
    let dir = tempfile::tempdir().unwrap();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
    for (from, to) in [
        ("smsgateway.conf.example", "smsgateway.conf"),
        ("sim.conf.example", "sim.conf"),
        ("integrations.conf.example", "integrations.conf"),
        ("addressbook.conf.example", "addressbook.conf"),
        ("anonymous.conf.example", "anonymous.conf"),
    ] {
        std::fs::copy(src.join(from), dir.path().join(to)).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    for f in ["sim.conf", "anonymous.conf"] {
        std::fs::set_permissions(dir.path().join(f), std::fs::Permissions::from_mode(0o640))
            .unwrap();
    }
    let mut o = LoadOptions::new(dir.path().join("smsgateway.conf"));
    o.use_env = false;
    let p = |n: &str| dir.path().join(n).display().to_string();
    o.overrides = vec![
        format!("paths.sim_config={}", p("sim.conf")),
        format!("paths.addressbook={}", p("addressbook.conf")),
        format!("paths.anonymous={}", p("anonymous.conf")),
        format!("integrations.config_file={}", p("integrations.conf")),
    ];
    config::load(&o).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn example_configuration_is_valid_and_matches_defaults() {
    let cfg = load_examples();
    assert!(
        cfg.warnings.is_empty(),
        "unexpected warnings: {:#?}",
        cfg.warnings
    );
    let d = Config::default();
    // The example documents the defaults; a mismatch means one of them is stale.
    assert_eq!(cfg.general, d.general);
    assert_eq!(cfg.modem, d.modem);
    assert_eq!(cfg.sms, d.sms);
    assert_eq!(cfg.mms, d.mms);
    assert_eq!(cfg.ussd, d.ussd);
    assert_eq!(cfg.database, d.database);
    assert_eq!(cfg.retention, d.retention);
    assert_eq!(cfg.api, d.api);
    assert_eq!(cfg.web, d.web);
    assert_eq!(cfg.logging, d.logging);
    assert_eq!(cfg.calls, d.calls);
    assert_eq!(cfg.gnss, d.gnss);
    assert_eq!(cfg.metrics, d.metrics);
    assert_eq!(cfg.security, d.security);
    assert_eq!(cfg.integrations.worker_count, d.integrations.worker_count);
    assert_eq!(cfg.anonymous, d.anonymous);
    assert_eq!(cfg.api.port, 8080);
    assert_eq!(cfg.modems[0].device, "/dev/ttyS0");
    // The integration examples from the documentation all parse, disabled.
    assert!(cfg.integration.len() >= 8);
    assert!(
        cfg.integration.iter().all(|i| !i.enabled),
        "examples must not forward anything"
    );
    for kind in [
        "webhook", "discord", "slack", "teams", "telegram", "ntfy", "gotify", "syslog",
    ] {
        assert!(
            cfg.integration.iter().any(|i| i.kind == kind),
            "missing {kind} example"
        );
    }
    assert_eq!(
        cfg.integration
            .iter()
            .filter(|i| i.kind == "discord")
            .count(),
        2
    );
    assert_eq!(cfg.contacts.len(), 3);
    assert_eq!(cfg.sims[0].mobile_number, "+4500000000");
}

#[test]
fn examples_contain_no_real_looking_secrets() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        if entry.path().is_file() {
            let text = std::fs::read_to_string(entry.path()).unwrap();
            for needle in [
                "github_pat_",
                "xoxb-",
                "hooks.slack.com/services/T",
                "discord.com/api/webhooks/1",
                "BEGIN PRIVATE KEY",
            ] {
                assert!(
                    !text.contains(needle),
                    "{} contains {needle}",
                    entry.path().display()
                );
            }
        }
    }
}
