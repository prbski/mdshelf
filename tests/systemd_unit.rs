//! The systemd unit `mdshelf install` registers must start `serve` with exactly the
//! arguments it was installed with.
//!
//! The service library writes `ExecStart=` by joining arguments with spaces, so a path
//! holding a space, a `%` or a `$` reaches systemd as several words, a specifier or a
//! variable unless mdshelf quotes it. This drives the library's real unit writer and
//! checks the file it produces.
//!
//! Kept in its own test binary with a single test: it points `HOME` at a temporary
//! directory so the library writes the unit there, and that must not race other tests.

use std::path::PathBuf;

use clap::Parser;
use mdshelf::cli::{Cli, Command};
use mdshelf::service::definition_for;
use service_manager::{ServiceManager, ServiceManagerKind, SystemdServiceManager};

#[test]
fn the_systemd_unit_keeps_every_argument_whole_and_literal() {
    let home = tempfile::tempdir().expect("temp home");
    // SAFETY: the only test in this binary, so nothing reads the environment concurrently.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
    }

    let argv = [
        "mdshelf",
        "install",
        "--config",
        "/srv/My Docs/mdshelf.toml",
        "--auth",
        "google",
        "--public-url",
        "https://docs.example.com",
        "--behind-proxy",
        "--tls-cert",
        "/etc/ssl/50% off/$HOST.pem",
        "--tls-key",
        "/etc/ssl/key.pem",
    ];
    let Command::Install(args) = Cli::try_parse_from(argv).expect("install parses").command else {
        panic!("parsed as another command");
    };

    let mut definition = definition_for(
        ServiceManagerKind::Systemd,
        PathBuf::from("/opt/md shelf/bin/mdshelf"),
        &args.serve,
        Some("/home/ana \"a\" 100%".to_string()),
    )
    .expect("definition builds");
    // Write the unit only: enabling it would call systemctl.
    definition.autostart = false;
    SystemdServiceManager::user()
        .install(definition)
        .expect("unit is written");

    let unit = walk(home.path())
        .into_iter()
        .find(|path| path.extension().is_some_and(|ext| ext == "service"))
        .expect("a .service file was written");
    let unit = std::fs::read_to_string(unit).expect("unit readable");
    let line = |key: &str| {
        unit.lines()
            .find(|line| line.starts_with(key))
            .unwrap_or_else(|| panic!("no {key} line in:\n{unit}"))
            .to_string()
    };

    assert_eq!(
        line("ExecStart="),
        concat!(
            r#"ExecStart="/opt/md shelf/bin/mdshelf" serve --config "/srv/My Docs/mdshelf.toml" "#,
            r#"--auth google --public-url https://docs.example.com "#,
            r#"--tls-cert "/etc/ssl/50%% off/$$HOST.pem" --tls-key /etc/ssl/key.pem --behind-proxy"#,
        )
    );
    assert_eq!(
        line("Environment="),
        r#"Environment="HOME=/home/ana \"a\" 100%%""#
    );
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("readable dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}
