//! Phase 2 — the ACL model (US-9 … US-14).
//!
//! Unit tests in `src/acl` cover the resolution algebra exhaustively. These tests check
//! the parts that only show up once real files, a real server, and the real binary are
//! involved: that rules are read from a vault on disk, that they never reach the
//! browser, that an edit takes effect immediately, and that the CLI reports them.

use std::path::Path;
use std::process::Command;

use mdshelf::test_support::{MockIdp, TestServer, client};

/// A vault exercising all three levels plus a deny.
const VAULT: &[(&str, &str)] = &[
    (
        "index.md",
        "---\ntitle: Home\nallow:\n  - team@corp.com\n---\n\n# Home\n",
    ),
    (
        "hr/index.md",
        "---\ntitle: HR\nallow:\n  - hr@corp.com\n---\n\n# HR\n",
    ),
    (
        "hr/comp.md",
        "---\ntitle: Compensation\ndeny:\n  - intern@corp.com\n---\n\n# Compensation\n",
    ),
    ("notes/idea.md", "---\ntitle: Idea\n---\n\n# Idea\n"),
];

fn write_vault(root: &Path, files: &[(&str, &str)]) {
    for (relative, contents) in files {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}

/// Lay out a config plus vault and return the config path.
fn scaffold(dir: &Path, files: &[(&str, &str)]) -> std::path::PathBuf {
    let vault = dir.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    write_vault(&vault, files);
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

// ---------------------------------------------------------------------------
// US-9: rules are read from the vault, and never leave it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rules_are_read_from_frontmatter_on_disk() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let universe = server.state.universe.read().await;
    let site = &universe.sites()[0];
    let acl = site.acl();

    assert!(acl.allows(Path::new("hr/comp.md"), "hr@corp.com"));
    assert!(!acl.allows(Path::new("hr/comp.md"), "intern@corp.com"));
    assert!(acl.allows(Path::new("notes/idea.md"), "team@corp.com"));
    assert!(!acl.allows(Path::new("notes/idea.md"), "stranger@corp.com"));
}

/// SEC-6. The single most damaging leak this feature could have: publishing the
/// invitee list to every reader of the page it protects.
#[tokio::test]
async fn invitee_addresses_never_appear_in_rendered_output() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let http = client();

    // Phase 3 adds the middleware that gates these responses; at this point the pages
    // render openly, which is precisely why the leak check is meaningful here.
    for path in ["/docs", "/docs/hr", "/docs/hr/comp", "/docs/notes/idea"] {
        let body = http
            .get(server.url(path))
            .send()
            .await
            .expect("page request")
            .text()
            .await
            .expect("page body");

        for address in ["team@corp.com", "hr@corp.com", "intern@corp.com"] {
            assert!(
                !body.contains(address),
                "{path} leaked {address} into rendered HTML"
            );
        }
        // The keys themselves must be gone from any serialized frontmatter too.
        assert!(
            !body.contains("\"allow\""),
            "{path} leaked an `allow` key into rendered HTML"
        );
        assert!(
            !body.contains("\"deny\""),
            "{path} leaked a `deny` key into rendered HTML"
        );
    }
}

#[tokio::test]
async fn a_malformed_rule_block_denies_everyone_named_in_it() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(
        &[
            (
                "index.md",
                "---\ntitle: Home\nallow:\n  - team@corp.com\n---\n\n# Home\n",
            ),
            (
                "broken.md",
                // Looks like two addresses; YAML reads it as one string.
                "---\ntitle: Broken\nallow: ana@corp.com, bob@corp.com\n---\n\n# Broken\n",
            ),
        ],
        &idp,
    )
    .await;

    let universe = server.state.universe.read().await;
    let acl = universe.sites()[0].acl();

    assert!(!acl.allows(Path::new("broken.md"), "ana@corp.com"));
    assert!(!acl.allows(Path::new("broken.md"), "bob@corp.com"));
    // D10: not even the site-level grant survives a broken block on the file.
    assert!(
        !acl.allows(Path::new("broken.md"), "team@corp.com"),
        "a poisoned file must not fall back to an inherited grant"
    );

    let poisoned = acl.poisoned();
    assert_eq!(poisoned.len(), 1);
    assert_eq!(poisoned[0].0, "broken.md");
    assert_eq!(poisoned[0].1.line, Some(3));
}

/// Regression: rules were collected from the rendered-page map, not from disk.
///
/// `hr.md` and `hr/index.md` both resolve to the URL `hr`, so the pages map — which is
/// keyed by URL — kept only one of them. Because the rule index was built from that
/// map, the loser's rules disappeared: a folder index's `deny` silently stopped
/// applying and its whole subtree fell back to whatever a broader rule granted.
///
/// A **fail-open**, and the second of that shape. The pages map is a presentation
/// structure; rules must come from every rule file that exists.
#[tokio::test]
async fn a_url_collision_does_not_discard_a_folder_rule() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(
        &[
            (
                "index.md",
                "---\ntitle: Home\nallow:\n  - team@corp.com\n---\n\n# Home\n",
            ),
            // Collides with hr/index.md on the URL `hr`.
            ("hr.md", "---\ntitle: HR overview\n---\n\n# Overview\n"),
            (
                "hr/index.md",
                "---\ntitle: HR\ndeny:\n  - team@corp.com\n---\n\n# HR\n",
            ),
            ("hr/secret.md", "---\ntitle: Secret\n---\n\n# ZZSECRETZZ\n"),
        ],
        &idp,
    )
    .await;

    let universe = server.state.universe.read().await;
    let acl = universe.sites()[0].acl();

    assert!(
        !acl.allows(Path::new("hr/secret.md"), "team@corp.com"),
        "the folder deny must survive a URL collision"
    );
    assert!(
        acl.rows()
            .iter()
            .any(|row| row.level == "folder" && row.path == "hr"),
        "the shadowed folder's rule is missing from the index: {:?}",
        acl.rows()
    );
}

/// The same principle for a file mdshelf cannot parse at all.
///
/// A rule block that fails *mdshelf's* validation was already poisoned (D10), but one
/// that fails the underlying YAML parser made the whole file vanish from the page map —
/// taking its rules with it. An unparseable `hr/index.md` therefore opened its subtree.
/// Unreadable must mean denied, not absent.
#[tokio::test]
async fn a_rule_file_that_cannot_be_parsed_denies_its_subtree() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(
        &[
            (
                "index.md",
                "---\ntitle: Home\nallow:\n  - team@corp.com\n---\n\n# Home\n",
            ),
            // Unclosed flow sequence: not valid YAML.
            (
                "hr/index.md",
                "---\ntitle: HR\ndeny: [team@corp.com\n---\n\n# HR\n",
            ),
            ("hr/secret.md", "---\ntitle: Secret\n---\n\n# ZZSECRETZZ\n"),
            // Tabs are illegal for YAML indentation.
            (
                "legal/index.md",
                "---\ntitle: Legal\ndeny:\n\t- team@corp.com\n---\n\n# Legal\n",
            ),
            ("legal/brief.md", "---\ntitle: Brief\n---\n\n# ZZBRIEFZZ\n"),
        ],
        &idp,
    )
    .await;

    let universe = server.state.universe.read().await;
    let acl = universe.sites()[0].acl();

    for page in ["hr/secret.md", "legal/brief.md"] {
        assert!(
            !acl.allows(Path::new(page), "team@corp.com"),
            "{page} was reachable despite an unparseable folder index"
        );
    }

    // And the operator is told which files to fix, rather than left guessing.
    let reported: Vec<&str> = acl.poisoned().iter().map(|(file, _)| *file).collect();
    assert!(reported.contains(&"hr/index.md"), "got: {reported:?}");
    assert!(reported.contains(&"legal/index.md"), "got: {reported:?}");
}

// ---------------------------------------------------------------------------
// US-12: an edit takes effect immediately
// ---------------------------------------------------------------------------

#[tokio::test]
async fn removing_an_address_denies_it_on_the_next_request() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    {
        let universe = server.state.universe.read().await;
        assert!(
            universe.sites()[0]
                .acl()
                .allows(Path::new("hr/policy.md"), "hr@corp.com")
        );
    }

    // The owner revokes access by editing the folder's index file.
    server
        .write_and_rebuild("hr/index.md", "---\ntitle: HR\nallow: []\n---\n\n# HR\n")
        .await;

    let universe = server.state.universe.read().await;
    assert!(
        !universe.sites()[0]
            .acl()
            .allows(Path::new("hr/policy.md"), "hr@corp.com"),
        "D3: revocation must take effect on the next request"
    );
}

#[tokio::test]
async fn deleting_a_file_removes_its_rule_with_no_residue() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    server.remove_and_rebuild("hr/comp.md").await;

    let universe = server.state.universe.read().await;
    let acl = universe.sites()[0].acl();
    // D4: the rule left with the file. Nothing is orphaned, and a file later created at
    // the same path starts from its inherited rules, not the deleted file's.
    let rows = acl.rows();
    assert!(
        !rows.iter().any(|row| row.path == "hr/comp.md"),
        "a deleted file must leave no rule behind: {rows:?}"
    );
}

#[tokio::test]
async fn renaming_a_file_carries_its_rule_along() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    let contents = std::fs::read_to_string(server.vault.join("hr/comp.md")).unwrap();
    std::fs::remove_file(server.vault.join("hr/comp.md")).unwrap();
    std::fs::write(server.vault.join("hr/2026-comp.md"), contents).unwrap();
    server.rebuild().await;

    let universe = server.state.universe.read().await;
    let acl = universe.sites()[0].acl();
    // D4: no rename detection is involved. The rule is inside the file.
    assert!(!acl.allows(Path::new("hr/2026-comp.md"), "intern@corp.com"));
    assert!(acl.allows(Path::new("hr/2026-comp.md"), "hr@corp.com"));
}

// ---------------------------------------------------------------------------
// US-13: `mdshelf check` gates malformed rules
// ---------------------------------------------------------------------------

#[test]
fn check_passes_on_a_vault_with_valid_rules() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);

    let output = run_mdshelf(&["check", "--config", config.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "check should pass:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("access rules:"));
}

#[test]
fn check_passes_unchanged_on_a_vault_with_no_rules() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(
        dir.path(),
        &[("index.md", "---\ntitle: Home\n---\n\n# Home\n")],
    );

    let output = run_mdshelf(&["check", "--config", config.to_str().unwrap()]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("total pages: 1"));
    assert!(
        !stdout.contains("access rules:"),
        "a rule-free vault should not grow new output"
    );
}

#[test]
fn check_fails_and_reports_every_malformed_rule() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(
        dir.path(),
        &[
            ("index.md", "---\ntitle: Home\n---\n\n# Home\n"),
            (
                "hr/comp.md",
                "---\ntitle: Comp\nallow: ana@corp.com, bob@corp.com\n---\n\n# Comp\n",
            ),
            (
                "hr/plan.md",
                "---\ntitle: Plan\nallow:\n  - ana@corp\n---\n\n# Plan\n",
            ),
        ],
    );

    let output = run_mdshelf(&["check", "--config", config.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "D31: malformed rules must fail the check"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("hr/comp.md:3"), "got:\n{stderr}");
    assert!(stderr.contains("must be a list"), "got:\n{stderr}");
    assert!(stderr.contains("hr/plan.md:4"), "got:\n{stderr}");
    assert!(stderr.contains("ana@corp"), "got:\n{stderr}");
    assert!(stderr.contains("2 access-rule error"), "got:\n{stderr}");
}

// ---------------------------------------------------------------------------
// US-14: `mdshelf acl explain`
// ---------------------------------------------------------------------------

#[test]
fn explain_traces_the_rule_that_decided() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);

    let output = run_mdshelf(&[
        "acl",
        "explain",
        "--config",
        config.to_str().unwrap(),
        "hr/comp.md",
        "intern@corp.com",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("hr/comp.md"), "got:\n{stdout}");
    assert!(stdout.contains("intern@corp.com"), "got:\n{stdout}");
    assert!(stdout.contains("DENY"), "got:\n{stdout}");
    assert!(
        stdout.contains("file rule in hr/comp.md"),
        "the verdict must name the deciding rule; got:\n{stdout}"
    );
}

#[test]
fn explain_reports_an_allow_from_an_inherited_rule() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);

    let output = run_mdshelf(&[
        "acl",
        "explain",
        "--config",
        config.to_str().unwrap(),
        "notes/idea.md",
        "team@corp.com",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ALLOW"), "got:\n{stdout}");
    assert!(stdout.contains("site rule in index.md"), "got:\n{stdout}");
}

#[test]
fn explain_says_when_the_fail_closed_default_decided() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);

    let output = run_mdshelf(&[
        "acl",
        "explain",
        "--config",
        config.to_str().unwrap(),
        "notes/idea.md",
        "stranger@elsewhere.com",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Silence is the reason, and saying so is the whole point of the command.
    assert!(
        stdout.contains("fail-closed default"),
        "an unexplained DENY is the failure mode this command exists to prevent; got:\n{stdout}"
    );
}

#[test]
fn explain_names_the_file_when_a_broken_rule_decided() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(
        dir.path(),
        &[
            (
                "index.md",
                "---\ntitle: Home\nallow:\n  - team@corp.com\n---\n\n# Home\n",
            ),
            (
                "broken.md",
                "---\ntitle: Broken\nallow: oops\n---\n\n# Broken\n",
            ),
        ],
    );

    let output = run_mdshelf(&[
        "acl",
        "explain",
        "--config",
        config.to_str().unwrap(),
        "broken.md",
        "team@corp.com",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("INVALID"), "got:\n{stdout}");
    assert!(stdout.contains("could not be parsed"), "got:\n{stdout}");
}

/// Regression: `acl explain` disagreed with the server about paths.
///
/// Rules are keyed on the on-disk path, but `explain` resolved the string the user
/// typed. On a case-insensitive filesystem `HR/comp.md` opens `hr/comp.md`, so the
/// server denied it via the folder rule while `explain` reported **ALLOW** from the
/// site rule — the wrong answer, in the direction of false reassurance, from the one
/// command whose purpose is telling you whether a page is locked down.
#[test]
fn explain_gives_the_same_verdict_however_the_path_is_spelled() {
    let dir = tempfile::tempdir().unwrap();
    // The folder must actually deny team@, so a lookup that misses the folder rule
    // visibly falls through to the site-level allow.
    let config = scaffold(
        dir.path(),
        &[
            (
                "index.md",
                "---\ntitle: Home\nallow:\n  - team@corp.com\n---\n\n# Home\n",
            ),
            (
                "hr/index.md",
                "---\ntitle: HR\ndeny:\n  - team@corp.com\n---\n\n# HR\n",
            ),
            ("hr/comp.md", "---\ntitle: Comp\n---\n\n# Comp\n"),
        ],
    );
    let config_arg = config.to_str().unwrap();

    // Every spelling of the same file must reach the folder rule.
    let mut verdicts = Vec::new();
    for spelling in [
        "hr/comp.md",
        "HR/comp.md",
        "Hr/COMP.md",
        // A case-variant extension: the server strips `.md` case-insensitively, so the
        // CLI must too. It did not, which is how this test grew.
        "hr/comp.MD",
        "/docs/hr/comp",
        "/docs/hr/comp.md",
        "/docs/HR/comp.MD",
        "/docs/HR/COMP",
        "hr/comp",
        "HR/comp",
    ] {
        let output = run_mdshelf(&[
            "acl",
            "explain",
            "--config",
            config_arg,
            spelling,
            "team@corp.com",
        ]);
        assert!(output.status.success(), "explain failed for {spelling}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let verdict = stdout
            .lines()
            .find(|line| line.starts_with("verdict:"))
            .unwrap_or("<none>")
            .to_string();
        verdicts.push((spelling, verdict));
    }

    let first = &verdicts[0].1;
    for (spelling, verdict) in &verdicts {
        assert_eq!(
            verdict, first,
            "{spelling} disagreed with the canonical spelling"
        );
    }
    assert!(
        first.contains("deny") && first.contains("hr/index.md"),
        "expected the folder rule to decide; got: {first}"
    );
}

#[test]
fn explain_rejects_an_invalid_address() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);

    let output = run_mdshelf(&[
        "acl",
        "explain",
        "--config",
        config.to_str().unwrap(),
        "hr/comp.md",
        "not-an-email",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not a valid email"),
        "got:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn explain_accepts_a_url_as_well_as_a_path() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);

    let output = run_mdshelf(&[
        "acl",
        "explain",
        "--config",
        config.to_str().unwrap(),
        "/docs/hr/comp",
        "intern@corp.com",
    ]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("hr/comp.md"));
}
