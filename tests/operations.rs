//! Phase 4 — operations (US-21, US-24, US-25).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use mdshelf::test_support::{MockIdp, TestServer, client};

const VAULT: &[(&str, &str)] = &[
    (
        "index.md",
        "---\ntitle: Handbook\nallow:\n  - team@corp.com\n---\n\n# Handbook\n",
    ),
    ("guide.md", "---\ntitle: Guide\n---\n\n# Guide\n"),
    (
        "hr/index.md",
        "---\ntitle: HR\nallow:\n  - hr@corp.com\ndeny:\n  - team@corp.com\n---\n\n# HR\n",
    ),
    ("hr/policy.md", "---\ntitle: Policy\n---\n\n# Policy\n"),
    ("assets/logo.png", "PNGDATA"),
];

fn scaffold(dir: &Path, files: &[(&str, &str)]) -> std::path::PathBuf {
    let vault = dir.join("vault");
    for (relative, contents) in files {
        let path = vault.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let config_path = dir.join("mdshelf.toml");
    std::fs::write(
        &config_path,
        "[[sites]]\npath = \"vault\"\nmount = \"/docs\"\ntitle = \"Docs\"\n",
    )
    .unwrap();
    config_path
}

fn run_mdshelf(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mdshelf"))
        .args(args)
        .output()
        .expect("running the mdshelf binary")
}

/// Every file in a tree with its contents, for before/after comparison.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if entry.file_type().is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .display()
                .to_string();
            files.insert(relative, std::fs::read(entry.path()).unwrap());
        }
    }
    files
}

// ---------------------------------------------------------------------------
// US-24: acl doctor
// ---------------------------------------------------------------------------

#[test]
fn doctor_reports_no_errors_on_a_healthy_vault() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);

    let output = run_mdshelf(&["acl", "doctor", "--config", config.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("no errors"));
}

#[test]
fn doctor_reports_malformed_blocks_and_exits_non_zero() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(
        dir.path(),
        &[
            ("index.md", "---\ntitle: Home\nallow:\n  - a@b.com\n---\n"),
            ("broken.md", "---\ntitle: Broken\nallow: not-a-list\n---\n"),
        ],
    );

    let output = run_mdshelf(&["acl", "doctor", "--config", config.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "hard errors must fail the command"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("broken.md:3"), "got:\n{stdout}");
}

#[test]
fn doctor_warns_when_a_vault_has_no_rules_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(
        dir.path(),
        &[("index.md", "---\ntitle: Home\n---\n\n# Home\n")],
    );

    let output = run_mdshelf(&["acl", "doctor", "--config", config.to_str().unwrap()]);
    // A warning, not an error: the vault is valid, it just has nothing granted. Under
    // the fail-closed default that means `--auth google` would show nobody anything,
    // which is exactly the surprise worth naming.
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no access rules at all"), "got:\n{stdout}");
    assert!(stdout.contains("invisible to everyone"), "got:\n{stdout}");
}

// ---------------------------------------------------------------------------
// US-25: acl grant, the only command that writes to a vault
// ---------------------------------------------------------------------------

#[test]
fn grant_adds_an_address_to_a_page() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);

    let output = run_mdshelf(&[
        "acl",
        "grant",
        "--config",
        config.to_str().unwrap(),
        "newcomer@corp.com",
        "hr/policy.md",
    ]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let updated = std::fs::read_to_string(dir.path().join("vault/hr/policy.md")).unwrap();
    assert!(
        updated.contains("allow:\n  - newcomer@corp.com"),
        "got:\n{updated}"
    );
    assert!(
        updated.contains("title: Policy"),
        "existing keys must survive"
    );
    assert!(updated.contains("# Policy"), "the body must survive");
}

#[test]
fn grant_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);
    let args = [
        "acl",
        "grant",
        "--config",
        config.to_str().unwrap(),
        "hr@corp.com",
        "hr/index.md",
    ];

    run_mdshelf(&args);
    let after_first = std::fs::read(dir.path().join("vault/hr/index.md")).unwrap();
    let output = run_mdshelf(&args);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("already listed"));
    assert_eq!(
        after_first,
        std::fs::read(dir.path().join("vault/hr/index.md")).unwrap(),
        "a repeated grant must not rewrite the file"
    );
}

#[test]
fn grant_creates_a_folder_index_only_with_consent() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(
        dir.path(),
        &[
            ("index.md", "---\ntitle: Home\nallow:\n  - a@b.com\n---\n"),
            ("legal/notes.md", "---\ntitle: Notes\n---\n\n# Notes\n"),
        ],
    );
    let index_path = dir.path().join("vault/legal/index.md");
    assert!(!index_path.exists());

    // Without --yes the prompt reads from a closed stdin, which counts as "no".
    let refused = run_mdshelf(&[
        "acl",
        "grant",
        "--config",
        config.to_str().unwrap(),
        "legal@corp.com",
        "legal",
    ]);
    assert!(!refused.status.success());
    assert!(
        !index_path.exists(),
        "D32: a file must not appear in the vault without consent"
    );

    let accepted = run_mdshelf(&[
        "acl",
        "grant",
        "--config",
        config.to_str().unwrap(),
        "--yes",
        "legal@corp.com",
        "legal",
    ]);
    assert!(
        accepted.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let created = std::fs::read_to_string(&index_path).unwrap();
    assert!(
        created.contains("allow:\n  - legal@corp.com"),
        "got:\n{created}"
    );
}

#[test]
fn grant_refuses_an_invalid_address_without_touching_the_vault() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);
    let before = snapshot(&dir.path().join("vault"));

    let output = run_mdshelf(&[
        "acl",
        "grant",
        "--config",
        config.to_str().unwrap(),
        "newcomer@corp",
        "guide.md",
    ]);
    assert!(!output.status.success());
    assert_eq!(before, snapshot(&dir.path().join("vault")));
}

/// The load-bearing half of D32: everything other than `acl grant` is read-only.
#[tokio::test]
async fn no_other_command_or_request_writes_to_the_vault() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let before = snapshot(&server.vault);

    // Serve a spread of requests: permitted, denied, missing, attachment, anonymous.
    let http = client();
    for path in [
        "/",
        "/docs",
        "/docs/guide",
        "/docs/hr/policy",
        "/docs/assets/logo.png",
        "/docs/nope",
        "/auth/login",
    ] {
        let _ = http.get(server.url(path)).send().await;
    }
    server.rebuild().await;

    assert_eq!(
        before,
        snapshot(&server.vault),
        "serving a vault must never modify it"
    );

    // And the read-only CLI commands.
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);
    let vault = dir.path().join("vault");
    let before = snapshot(&vault);

    let config_arg = config.to_str().unwrap();
    run_mdshelf(&["check", "--config", config_arg]);
    run_mdshelf(&["acl", "doctor", "--config", config_arg]);
    run_mdshelf(&[
        "acl",
        "explain",
        "--config",
        config_arg,
        "guide.md",
        "team@corp.com",
    ]);
    run_mdshelf(&[
        "export",
        "--config",
        config_arg,
        "--output",
        dir.path().join("out").to_str().unwrap(),
        "--as",
        "team@corp.com",
    ]);

    assert_eq!(
        before,
        snapshot(&vault),
        "check, doctor, explain, and export must all be read-only"
    );
}

// ---------------------------------------------------------------------------
// US-21: the access log
// ---------------------------------------------------------------------------

#[test]
fn audit_explains_itself_when_no_log_exists_yet() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);

    let output = run_mdshelf(&[
        "audit",
        "--config",
        config.to_str().unwrap(),
        "--path",
        "/docs/guide",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no access log"), "got:\n{stderr}");
    assert!(stderr.contains("--auth google"), "got:\n{stderr}");
}

#[tokio::test]
async fn requests_are_recorded_against_the_reader_who_made_them() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth enabled");

    // Sign in, then read one permitted page and one restricted page.
    let http = client();
    let login = http.get(server.url("/auth/login")).send().await.unwrap();
    let authorize_url = login
        .headers()
        .get(reqwest::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let state = url::Url::parse(&authorize_url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    idp.register_code(
        &format!("code-for-{state}"),
        mdshelf::test_support::TokenSpec::valid("team@corp.com"),
    );
    let authorize = http.get(&authorize_url).send().await.unwrap();
    let callback = http
        .get(
            authorize
                .headers()
                .get(reqwest::header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();
    let cookie = callback
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|h| {
            let rest = h.strip_prefix("mdshelf_session=")?;
            Some(rest.split(';').next()?.to_string())
        })
        .unwrap();

    for path in ["/docs/guide", "/docs/hr/policy"] {
        let _ = http
            .get(server.url(path))
            .header(reqwest::header::COOKIE, format!("mdshelf_session={cookie}"))
            .send()
            .await;
    }

    let entries = runtime.store.access_by_email("team@corp.com").unwrap();
    assert!(entries.len() >= 2, "expected both reads to be logged");

    let allowed = entries.iter().find(|e| e.path == "/docs/guide").unwrap();
    assert_eq!(allowed.outcome, "allow");
    let denied = entries
        .iter()
        .find(|e| e.path == "/docs/hr/policy")
        .unwrap();
    assert_eq!(
        denied.outcome, "deny",
        "a refused read is the entry an owner most wants to see"
    );
}

/// US-21: the audit output must distinguish a read from a refusal.
///
/// It did not. Both were listed identically, so "who has seen this document" silently
/// included everyone who had been *denied* it — the opposite conclusion, in the one
/// situation where the answer matters. Found by auditing the acceptance criteria rather
/// than by hunting for bugs.
#[test]
fn audit_distinguishes_reads_from_refusals() {
    use mdshelf::auth::store::{Outcome, Store, now_ms};

    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);

    let store = Store::open(&dir.path().join("mdshelf.db")).unwrap();
    let now = now_ms();
    store
        .log_access("hr@corp.com", "/docs/hr/policy", now, Outcome::Allow)
        .unwrap();
    store
        .log_access("team@corp.com", "/docs/hr/policy", now, Outcome::Deny)
        .unwrap();
    drop(store);

    let output = run_mdshelf(&[
        "audit",
        "--config",
        config.to_str().unwrap(),
        "--path",
        "/docs/hr/policy",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The person who was refused must not read as though they had access.
    let refused_line = stdout
        .lines()
        .find(|line| line.contains("team@corp.com"))
        .expect("the refused attempt should be listed");
    assert!(
        refused_line.contains("REFUSED"),
        "a denied attempt was reported indistinguishably from a read: {refused_line}"
    );

    let read_line = stdout
        .lines()
        .find(|line| line.contains("hr@corp.com"))
        .expect("the successful read should be listed");
    assert!(read_line.contains("read"), "got: {read_line}");
    assert!(stdout.contains("1 read, 1 refused"), "got:\n{stdout}");
}

/// US-21: the query commands work against a real log.
#[test]
fn audit_queries_by_path_and_email_and_can_erase() {
    use mdshelf::auth::store::{Outcome, Store, now_ms};

    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);
    let config_arg = config.to_str().unwrap();

    let store = Store::open(&dir.path().join("mdshelf.db")).unwrap();
    let now = now_ms();
    store
        .log_access("ana@corp.com", "/docs/guide", now, Outcome::Allow)
        .unwrap();
    store
        .log_access("bob@corp.com", "/docs/guide", now, Outcome::Allow)
        .unwrap();
    drop(store);

    let by_path = run_mdshelf(&["audit", "--config", config_arg, "--path", "/docs/guide"]);
    let stdout = String::from_utf8_lossy(&by_path.stdout);
    assert!(stdout.contains("ana@corp.com") && stdout.contains("bob@corp.com"));

    let by_email = run_mdshelf(&["audit", "--config", config_arg, "--email", "ana@corp.com"]);
    let stdout = String::from_utf8_lossy(&by_email.stdout);
    assert!(stdout.contains("/docs/guide"));
    assert!(
        !stdout.contains("bob@corp.com"),
        "the filter should exclude others"
    );

    // GDPR erasure removes that person and leaves everyone else intact (D27).
    let forget = run_mdshelf(&[
        "audit",
        "--config",
        config_arg,
        "--email",
        "ana@corp.com",
        "--forget",
    ]);
    assert!(forget.status.success());
    assert!(String::from_utf8_lossy(&forget.stdout).contains("removed 1 log entr"));

    let after = run_mdshelf(&["audit", "--config", config_arg, "--email", "ana@corp.com"]);
    assert!(String::from_utf8_lossy(&after.stdout).contains("no access log entries"));

    let others = run_mdshelf(&["audit", "--config", config_arg, "--email", "bob@corp.com"]);
    assert!(
        String::from_utf8_lossy(&others.stdout).contains("/docs/guide"),
        "erasing one person must not erase anyone else"
    );
}

/// US-24: the advisory branches, none of which had coverage.
#[test]
fn doctor_reports_unreachable_subtrees_unused_grants_and_missing_indexes() {
    use mdshelf::auth::store::{Outcome, Store, now_ms};

    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(
        dir.path(),
        &[
            (
                "index.md",
                "---\ntitle: Home\nallow:\n  - team@corp.com\n  - never@corp.com\n---\n\n# Home\n",
            ),
            // Denies every address the vault names, so nobody can reach it.
            (
                "orphan/index.md",
                "---\ntitle: Orphan\ndeny:\n  - team@corp.com\n  - never@corp.com\n---\n\n# Orphan\n",
            ),
            // A folder with no index file of its own.
            ("loose/page.md", "---\ntitle: Loose\n---\n\n# Loose\n"),
        ],
    );

    // A log in which team@ has read something but never@ never has.
    let store = Store::open(&dir.path().join("mdshelf.db")).unwrap();
    store
        .log_access("team@corp.com", "/docs", now_ms(), Outcome::Allow)
        .unwrap();
    drop(store);

    let output = run_mdshelf(&["acl", "doctor", "--config", config.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "advisories are warnings, not errors"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("orphan") && stdout.contains("no address named in this vault can read"),
        "unreachable subtree not reported:\n{stdout}"
    );
    assert!(
        stdout.contains("never@corp.com") && stdout.contains("never seen in the access log"),
        "unused grant not reported:\n{stdout}"
    );
    assert!(
        stdout.contains("loose/") && stdout.contains("no index.md"),
        "folder without an index not reported:\n{stdout}"
    );
    // Somebody who *has* used their grant must not be flagged.
    assert!(
        !stdout.contains("team@corp.com  granted"),
        "an exercised grant was reported as unused:\n{stdout}"
    );
}

#[test]
fn the_access_log_is_pruned_past_its_retention_window() {
    use mdshelf::auth::store::{Outcome, Store};
    use std::time::Duration;

    let store = Store::open_in_memory().unwrap();
    let now = 1_800_000_000_000i64;
    store
        .log_access(
            "ana@corp.com",
            "/docs/a",
            now - 100 * 86_400_000,
            Outcome::Allow,
        )
        .unwrap();
    store
        .log_access(
            "ana@corp.com",
            "/docs/b",
            now - 10 * 86_400_000,
            Outcome::Allow,
        )
        .unwrap();

    let removed = store
        .prune_access_log(now, Duration::from_secs(90 * 86_400))
        .unwrap();
    assert_eq!(removed, 1, "only the entry past 90 days should go");

    let remaining = store.access_by_email("ana@corp.com").unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].path, "/docs/b");
}
