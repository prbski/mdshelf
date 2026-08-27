//! Phase 1 — the link store and the `share` commands (US-1 … US-6).
//!
//! Everything here drives the real binary against a real vault, because the acceptance
//! criteria are about what an operator sees on stdout and what ends up in the database,
//! not about what the functions return.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use mdshelf::auth::store::Store;
use mdshelf::links::token_hash;

const VAULT: &[(&str, &str)] = &[
    (
        "index.md",
        "---\ntitle: Handbook\nallow:\n  - ana@corp.com\n---\n\n# Handbook\n",
    ),
    (
        "hr/comp.md",
        "---\ntitle: Compensation\n---\n\n# Compensation\n\nSalary bands.\n",
    ),
    ("hr/chart.png", "PNGDATA"),
];

/// A vault plus a config that enables auth and links.
fn scaffold(dir: &Path, extra_config: &str) -> PathBuf {
    let vault = dir.join("vault");
    for (relative, contents) in VAULT {
        let path = vault.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let config_path = dir.join("mdshelf.toml");
    std::fs::write(
        &config_path,
        format!(
            "host = \"127.0.0.1\"\nport = 4444\n\n\
             [auth]\nowner_email = \"ana@corp.com\"\n\n\
             {extra_config}\n\
             [[sites]]\npath = \"vault\"\nmount = \"/docs\"\ntitle = \"Docs\"\n"
        ),
    )
    .unwrap();
    config_path
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mdshelf"))
        .args(args)
        .output()
        .expect("running the mdshelf binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// Mint a link and return the URL it printed.
fn share(config: &Path, args: &[&str]) -> String {
    let mut all = vec!["share"];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--config", config.to_str().unwrap()]);
    let output = run(&all);
    assert!(output.status.success(), "share failed: {}", stderr(&output));
    stdout(&output).trim().to_string()
}

fn token_of(url: &str) -> &str {
    url.rsplit('/').next().expect("a token at the end")
}

fn store_at(config: &Path) -> Store {
    Store::open(&config.parent().unwrap().join("mdshelf.db")).expect("opening the store")
}

// ---------------------------------------------------------------------------
// US-1 — create a link from the CLI
// ---------------------------------------------------------------------------

#[test]
fn share_prints_exactly_one_url_with_no_path_component() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");

    let output = run(&[
        "share",
        "/docs/hr/comp",
        "--for",
        "1d",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));

    let printed = stdout(&output);
    let lines: Vec<&str> = printed.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one line of stdout: {printed:?}");

    let url = lines[0];
    let token = token_of(url);
    assert_eq!(
        url,
        format!("http://127.0.0.1:4444/s/{token}"),
        "the URL is <public_url><prefix>/<token> and nothing else"
    );
    // S4: nothing in the URL names the site, the folder or the page.
    assert!(!url.contains("/docs"), "got: {url}");
    assert!(!url.contains("comp"), "got: {url}");
}

#[test]
fn the_row_holds_the_site_root_the_relative_path_the_expiry_and_the_issuer() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");
    let url = share(&config, &["/docs/hr/comp", "--for", "1d"]);

    let store = store_at(&config);
    let record = store
        .link_by_token_hash(&token_hash(token_of(&url)))
        .unwrap()
        .expect("the token hash finds its row");

    assert_eq!(record.path, "hr/comp.md");
    assert!(
        record.site.ends_with("vault"),
        "the site key is its root path: {}",
        record.site
    );
    assert_eq!(record.issued_by, "ana@corp.com");
    assert!(record.revoked_at.is_none());
    let lifetime = record.expires_at - record.created_at;
    assert!(
        (86_000_000..=86_500_000).contains(&lifetime),
        "a 1d link should last about a day, got {lifetime}ms"
    );
}

/// US-1: `--for 45d` against a 30d cap exits non-zero and mints nothing.
#[test]
fn a_lifetime_beyond_the_cap_mints_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "[links]\nmax_lifetime = \"30d\"\n");

    let output = run(&[
        "share",
        "/docs/hr/comp",
        "--for",
        "45d",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "45d must be refused");
    assert!(
        stderr(&output).contains("max_lifetime"),
        "{}",
        stderr(&output)
    );

    // The precondition that makes the absence meaningful: a link *inside* the cap does
    // get created against the same config. Before that, nothing has ever been minted,
    // so there is not even a database to list.
    let listing = run(&[
        "share",
        "list",
        "--all",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(!listing.status.success(), "{}", stdout(&listing));
    assert!(
        stderr(&listing).contains("no link database"),
        "{}",
        stderr(&listing)
    );
    share(&config, &["/docs/hr/comp", "--for", "29d"]);
    let listing = run(&["share", "list", "--config", config.to_str().unwrap()]);
    assert!(
        stdout(&listing).contains("1 link(s)"),
        "{}",
        stdout(&listing)
    );
}

#[test]
fn until_stores_the_named_instant_as_utc() {
    let dir = tempfile::tempdir().unwrap();
    // A cap wide enough that a far-future date is about `--until`, not about the cap.
    let config = scaffold(dir.path(), "[links]\nmax_lifetime = \"36500d\"\n");
    let url = share(&config, &["/docs/hr/comp", "--until", "2099-09-01"]);

    let store = store_at(&config);
    let record = store
        .link_by_token_hash(&token_hash(token_of(&url)))
        .unwrap()
        .unwrap();
    assert_eq!(
        mdshelf::links::time::format_instant(record.expires_at),
        "2099-09-01 23:59:59Z"
    );

    // An explicit offset moves the stored instant rather than being ignored.
    let url = share(
        &config,
        &["/docs/hr/comp", "--until", "2099-09-01T08:00:00+02:00"],
    );
    let record = store
        .link_by_token_hash(&token_hash(token_of(&url)))
        .unwrap()
        .unwrap();
    assert_eq!(
        mdshelf::links::time::format_instant(record.expires_at),
        "2099-09-01 06:00:00Z"
    );
}

#[test]
fn for_and_until_together_are_refused_and_neither_uses_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "[links]\ndefault_lifetime = \"2h\"\n");

    let output = run(&[
        "share",
        "/docs/hr/comp",
        "--for",
        "1d",
        "--until",
        "2099-09-01",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "two ways to say the same thing");

    let url = share(&config, &["/docs/hr/comp"]);
    let store = store_at(&config);
    let record = store
        .link_by_token_hash(&token_hash(token_of(&url)))
        .unwrap()
        .unwrap();
    let lifetime = record.expires_at - record.created_at;
    assert!(
        (7_100_000..=7_300_000).contains(&lifetime),
        "default_lifetime = 2h should govern, got {lifetime}ms"
    );
}

#[test]
fn a_path_that_does_not_exist_names_the_closest_match() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");

    let output = run(&[
        "share",
        "/docs/hr/COMP",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("/docs/hr/comp"),
        "a case-only difference must be named: {}",
        stderr(&output)
    );

    // A path with no near neighbour still fails, just without a suggestion.
    let output = run(&[
        "share",
        "/docs/hr/nothing-like-this",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(
        !stderr(&output).contains("Did you mean"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_path_outside_every_site_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");
    let output = run(&[
        "share",
        "/elsewhere/secret",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no configured site"),
        "{}",
        stderr(&output)
    );
}

/// US-1/S16: without auth every page is already public, so a link would imply a
/// protection that is not there.
#[test]
fn share_without_auth_configured_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    for (relative, contents) in VAULT {
        let path = vault.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let config = dir.path().join("mdshelf.toml");
    std::fs::write(
        &config,
        "[[sites]]\npath = \"vault\"\nmount = \"/docs\"\ntitle = \"Docs\"\n",
    )
    .unwrap();

    for args in [
        vec!["share", "/docs/hr/comp"],
        vec!["share", "list"],
        vec!["share", "revoke", "--all"],
    ] {
        let mut all = args.clone();
        all.extend_from_slice(&["--config", config.to_str().unwrap()]);
        let output = run(&all);
        assert!(!output.status.success(), "{args:?} must be refused");
        assert!(stderr(&output).contains("[auth]"), "{}", stderr(&output));
    }
    assert!(
        !dir.path().join("mdshelf.db").exists(),
        "a refused share must not create a database"
    );
}

#[test]
fn an_undeterminable_public_url_is_refused_unless_base_url_is_given() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    for (relative, contents) in VAULT {
        let path = vault.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let config = dir.path().join("mdshelf.toml");
    std::fs::write(
        &config,
        "host = \"0.0.0.0\"\nport = 4444\n\n[auth]\nowner_email = \"ana@corp.com\"\n\n\
         [[sites]]\npath = \"vault\"\nmount = \"/docs\"\ntitle = \"Docs\"\n",
    )
    .unwrap();

    let output = run(&[
        "share",
        "/docs/hr/comp",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(
        !output.status.success(),
        "0.0.0.0 is not a URL anyone visits"
    );
    assert!(
        stderr(&output).contains("--base-url"),
        "{}",
        stderr(&output)
    );

    let output = run(&[
        "share",
        "/docs/hr/comp",
        "--base-url",
        "https://docs.acme.com",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output)
            .trim()
            .starts_with("https://docs.acme.com/s/"),
        "{}",
        stdout(&output)
    );
}

// ---------------------------------------------------------------------------
// US-2 — tokens are unguessable and hashed at rest
// ---------------------------------------------------------------------------

/// SEC-1/SEC-2: the database file holds the hash, never the token.
#[test]
fn the_database_file_does_not_contain_a_freshly_minted_token() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");
    let url = share(&config, &["/docs/hr/comp", "--for", "1d"]);
    let token = token_of(&url);

    // The precondition: the row really is there, found by hash.
    let store = store_at(&config);
    assert!(
        store
            .link_by_token_hash(&token_hash(token))
            .unwrap()
            .is_some(),
        "the link must exist for its absence in the file to mean anything"
    );
    store.checkpoint().unwrap();
    drop(store);

    let needle = token.as_bytes();
    for entry in walkdir::WalkDir::new(dir.path()).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let bytes = std::fs::read(entry.path()).unwrap_or_default();
        assert!(
            !bytes.windows(needle.len()).any(|window| window == needle),
            "{} contains the plaintext token",
            entry.path().display()
        );
    }
}

/// US-2/SEC-2: no log line at any level, and no error message, carries the token.
#[test]
fn no_log_line_or_error_message_contains_a_plaintext_token() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");

    let output = Command::new(env!("CARGO_BIN_EXE_mdshelf"))
        .args([
            "-vv",
            "share",
            "/docs/hr/comp",
            "--for",
            "1d",
            "--config",
            config.to_str().unwrap(),
        ])
        .env("MDSHELF_LOG", "trace")
        .output()
        .expect("running the mdshelf binary");
    assert!(output.status.success(), "{}", stderr(&output));

    let url = stdout(&output).trim().to_string();
    let token = token_of(&url).to_string();
    assert!(!token.is_empty());
    assert!(
        !stderr(&output).contains(&token),
        "the token reached stderr at trace level:\n{}",
        stderr(&output)
    );

    // Every later command that handles the same link must stay quiet about it too.
    for args in [
        vec!["share", "list", "--all"],
        vec!["share", "list", "--json"],
    ] {
        let mut all = vec!["-vv"];
        all.extend_from_slice(&args);
        all.extend_from_slice(&["--config", config.to_str().unwrap()]);
        let output = Command::new(env!("CARGO_BIN_EXE_mdshelf"))
            .args(&all)
            .env("MDSHELF_LOG", "trace")
            .output()
            .unwrap();
        let combined = format!("{}{}", stdout(&output), stderr(&output));
        assert!(!combined.contains(&token), "{args:?} leaked the token");
    }
}

// ---------------------------------------------------------------------------
// US-3 — list
// ---------------------------------------------------------------------------

#[test]
fn list_shows_live_links_and_all_adds_the_dead_ones() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");
    let live = share(&config, &["/docs/hr/comp", "--for", "1d"]);
    let doomed = share(&config, &["/docs", "--for", "1d"]);

    let store = store_at(&config);
    let doomed_id = store
        .link_by_token_hash(&token_hash(token_of(&doomed)))
        .unwrap()
        .unwrap()
        .id;
    let live_id = store
        .link_by_token_hash(&token_hash(token_of(&live)))
        .unwrap()
        .unwrap()
        .id;
    drop(store);

    run(&[
        "share",
        "revoke",
        &doomed_id,
        "--config",
        config.to_str().unwrap(),
    ]);

    let listing = stdout(&run(&[
        "share",
        "list",
        "--config",
        config.to_str().unwrap(),
    ]));
    assert!(listing.contains(&live_id), "{listing}");
    assert!(!listing.contains(&doomed_id), "a revoked link is not live");
    assert!(listing.contains("ana@corp.com"), "{listing}");
    assert!(listing.contains("/docs/hr/comp"), "{listing}");

    let listing = stdout(&run(&[
        "share",
        "list",
        "--all",
        "--config",
        config.to_str().unwrap(),
    ]));
    assert!(listing.contains(&doomed_id), "{listing}");
    assert!(listing.contains("revoked"), "{listing}");
}

#[test]
fn list_json_is_valid_and_carries_no_token() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");
    let url = share(&config, &["/docs/hr/comp", "--for", "1d"]);
    let token = token_of(&url);

    let output = run(&[
        "share",
        "list",
        "--json",
        "--config",
        config.to_str().unwrap(),
    ]);
    let body = stdout(&output);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    let rows = parsed.as_array().expect("an array of links");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["path"], "hr/comp.md");
    assert_eq!(rows[0]["issued_by"], "ana@corp.com");
    assert_eq!(rows[0]["state"], "live");
    assert!(!body.contains(token), "the inventory must carry no token");
    assert!(!body.contains("token_hash"), "nor the hash");
}

// ---------------------------------------------------------------------------
// US-4 — revoke
// ---------------------------------------------------------------------------

#[test]
fn revoke_is_idempotent_and_refuses_an_unknown_id() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");
    let url = share(&config, &["/docs/hr/comp", "--for", "1d"]);
    let store = store_at(&config);
    let id = store
        .link_by_token_hash(&token_hash(token_of(&url)))
        .unwrap()
        .unwrap()
        .id;
    drop(store);

    let output = run(&["share", "revoke", &id, "--config", config.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));

    let store = store_at(&config);
    let first = store.link_by_id(&id).unwrap().unwrap().revoked_at;
    assert!(first.is_some());
    drop(store);

    let output = run(&["share", "revoke", &id, "--config", config.to_str().unwrap()]);
    assert!(output.status.success(), "revoking twice is not an error");
    let store = store_at(&config);
    assert_eq!(
        store.link_by_id(&id).unwrap().unwrap().revoked_at,
        first,
        "the second revoke must change nothing"
    );
    drop(store);

    let output = run(&[
        "share",
        "revoke",
        "ffffff",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "an unknown id is an error");
}

#[test]
fn revoke_all_reports_the_count() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");
    share(&config, &["/docs/hr/comp", "--for", "1d"]);
    share(&config, &["/docs", "--for", "1d"]);

    let output = run(&[
        "share",
        "revoke",
        "--all",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains('2'), "{}", stdout(&output));

    let listing = stdout(&run(&[
        "share",
        "list",
        "--config",
        config.to_str().unwrap(),
    ]));
    assert!(listing.contains("no share links"), "{listing}");

    // Running it again is harmless and reports nothing left to do.
    let output = run(&[
        "share",
        "revoke",
        "--all",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(output.status.success());
    assert!(stdout(&output).contains('0'), "{}", stdout(&output));
}

// ---------------------------------------------------------------------------
// US-6 — configuration and schema
// ---------------------------------------------------------------------------

/// A version-1 database gains the table additively; its existing rows are untouched.
#[test]
fn a_version_one_database_upgrades_without_losing_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mdshelf.db");

    {
        // Exactly the version-1 schema, written by hand.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE mdshelf_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE sessions (id TEXT PRIMARY KEY, email TEXT NOT NULL,
                 created_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL,
                 refresh_token_enc BLOB);
             CREATE TABLE access_log (email TEXT NOT NULL, path TEXT NOT NULL,
                 ts INTEGER NOT NULL, outcome TEXT NOT NULL);
             CREATE TABLE rules_index (site TEXT NOT NULL, path TEXT NOT NULL,
                 level TEXT NOT NULL, effect TEXT NOT NULL, email TEXT NOT NULL);
             INSERT INTO mdshelf_meta VALUES ('schema_version', 1);
             INSERT INTO sessions VALUES ('sid-1', 'ana@corp.com', 1, 1, NULL);
             INSERT INTO access_log VALUES ('ana@corp.com', '/docs/a', 1, 'allow');",
        )
        .unwrap();
    }

    let store = Store::open(&path).expect("a version-1 database must upgrade in place");
    assert_eq!(store.count_sessions().unwrap(), 1, "sessions survive");
    assert_eq!(
        store.access_by_path("/docs/a").unwrap().len(),
        1,
        "history survives"
    );
    // The new table exists and is usable.
    store
        .insert_link(
            "ab12cd",
            &[1u8; 32],
            "/v",
            "a.md",
            10_000,
            1,
            "ana@corp.com",
        )
        .unwrap();
    assert!(store.link_by_id("ab12cd").unwrap().is_some());
}

/// R3: the newer-schema error may no longer claim a deletion is cheap.
#[test]
fn a_newer_schema_error_names_share_links_among_what_would_be_lost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mdshelf.db");
    {
        let store = Store::open(&path).unwrap();
        drop(store);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE mdshelf_meta SET value = 3 WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    }
    let message = match Store::open(&path) {
        Ok(_) => panic!("a future schema version must be refused"),
        Err(error) => error.to_string(),
    };
    assert!(message.contains("share link"), "got: {message}");
}

// ---------------------------------------------------------------------------
// US-11 — link reads sit alongside signed-in reads in `mdshelf audit`
// ---------------------------------------------------------------------------

#[test]
fn audit_shows_link_reads_alongside_signed_in_reads() {
    use mdshelf::auth::store::Outcome;

    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");
    let url = share(&config, &["/docs/hr/comp", "--for", "1d"]);

    let store = store_at(&config);
    let id = store
        .link_by_token_hash(&token_hash(token_of(&url)))
        .unwrap()
        .unwrap()
        .id;
    let now = mdshelf::auth::store::now_ms();
    store
        .log_access("ana@corp.com", "/docs/hr/comp", now, Outcome::Allow)
        .unwrap();
    store
        .log_access(&format!("link:{id}"), "/docs/hr/comp", now, Outcome::Allow)
        .unwrap();
    store
        .log_access("link:unknown", "/s", now, Outcome::BadLink)
        .unwrap();
    drop(store);

    let output = run(&[
        "audit",
        "--path",
        "/docs/hr/comp",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let listing = stdout(&output);
    assert!(
        listing.contains("ana@corp.com"),
        "a signed-in read: {listing}"
    );
    assert!(
        listing.contains(&format!("link:{id}")),
        "a link read, under the same id `share list` prints: {listing}"
    );
    assert!(listing.contains("2 read"), "{listing}");

    // The bad-link row is reported as its own outcome rather than as a refusal, so a
    // stranger scanning the prefix does not read as a colleague being turned away.
    let probes = run(&[
        "audit",
        "--path",
        "/s",
        "--config",
        config.to_str().unwrap(),
    ]);
    let listing = stdout(&probes);
    assert!(listing.contains("BAD-LINK"), "{listing}");
    assert!(listing.contains("1 unknown link(s)"), "{listing}");
    assert!(
        !listing.contains(token_of(&url)),
        "no token in the audit output"
    );
}

/// R5: sharing is a property of the running theme, so a theme that never places the
/// control is worth naming once rather than leaving somebody to wonder.
#[test]
fn check_notes_a_theme_that_does_not_place_the_share_control() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), "");

    // The bundled theme ships it, so a plain check says nothing.
    let bundled = run(&["check", "--config", config.to_str().unwrap()]);
    assert!(bundled.status.success(), "{}", stderr(&bundled));
    assert!(
        !stdout(&bundled).contains("share_control"),
        "the bundled theme has the control: {}",
        stdout(&bundled)
    );

    // A theme override that replaces the header without it does.
    let theme = dir.path().join("theme");
    std::fs::create_dir_all(theme.join("partials")).unwrap();
    std::fs::write(theme.join("partials/header.html"), "<header></header>\n").unwrap();
    let config_body = std::fs::read_to_string(&config)
        .unwrap()
        .replace("[auth]", "[theme]\ndirectory = \"theme\"\n\n[auth]");
    std::fs::write(&config, config_body).unwrap();

    let stripped = run(&["check", "--config", config.to_str().unwrap()]);
    assert!(stripped.status.success(), "{}", stderr(&stripped));
    assert!(
        stdout(&stripped).contains("share_control"),
        "the missing control should be reported: {}",
        stdout(&stripped)
    );
    assert!(
        stdout(&stripped).contains("command line is unaffected"),
        "and the CLI escape hatch named"
    );
}
