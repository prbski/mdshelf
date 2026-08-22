//! US-26: the documentation criteria, as a drift guard.
//!
//! These are the only acceptance criteria whose subject is prose, so nothing else can
//! check them. The risk is not that the docs are wrong today — it is that someone
//! reorganises README.md in six months and quietly drops the paragraph explaining that
//! a vault with no rules shows nobody anything, or that a Google outage signs readers
//! out. Those two are the surprises most likely to generate a bug report.
//!
//! Deliberately checks for *substance*, not exact wording, so the docs stay editable.

/// Collapse runs of whitespace so a phrase still matches after the prose is rewrapped.
/// Checking substance must not mean freezing the line breaks.
fn flattened(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn readme() -> String {
    flattened(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
            .expect("README.md"),
    )
}

fn development() -> String {
    flattened(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/DEVELOPMENT.md"))
            .expect("DEVELOPMENT.md"),
    )
}

#[test]
fn the_readme_documents_the_frontmatter_rule_syntax() {
    let readme = readme();
    assert!(readme.contains("allow:"), "the rule syntax must be shown");
    assert!(readme.contains("deny:"));
    // D8's sharp edge: an index file governs its whole folder, which is the single
    // most surprising part of the model.
    assert!(
        readme.contains("index.md") && readme.to_lowercase().contains("everything beneath it"),
        "the index.md rule must be stated explicitly"
    );
    // The list-vs-string strictness that turns a typo into a denial.
    assert!(
        readme.contains("bare string"),
        "the strict-list requirement should be documented"
    );
}

#[test]
fn the_readme_does_not_bury_the_fail_closed_default() {
    let readme = readme();
    assert!(
        readme.contains("Fail closed"),
        "the fail-closed default needs its own heading, not a footnote"
    );
    assert!(
        readme.contains("shows nobody anything"),
        "a vault with no rules being invisible is the first surprise a new user hits"
    );
}

/// R1, the accepted risk that will generate support questions if undocumented.
#[test]
fn the_readme_documents_the_google_availability_dependency() {
    let readme = readme();
    assert!(
        readme.contains("Google is a hard dependency"),
        "R1 must be stated plainly"
    );
    assert!(
        readme.contains("unreachable"),
        "the doc should say that an unreachable Google also ends sessions, not just a rejection"
    );
    assert!(
        readme.contains("logged at WARN"),
        "readers need to know the event is diagnosable from the logs"
    );
}

#[test]
fn the_readme_covers_every_tls_route_and_the_credentials_setup() {
    let readme = readme();
    for route in ["--domain", "--tls-cert", "--behind-proxy"] {
        assert!(readme.contains(route), "TLS route {route} is undocumented");
    }
    assert!(readme.contains("mdshelf auth setup"));
    assert!(
        readme.contains("MDSHELF_GOOGLE_CLIENT_ID"),
        "the environment override should be documented"
    );
    assert!(
        readme.contains("credentials.env"),
        "where credentials are stored should be documented"
    );
}

#[test]
fn the_readme_explains_export_and_the_read_only_guarantee() {
    let readme = readme();
    assert!(
        readme.contains("--as"),
        "viewer-scoped export must be documented"
    );
    assert!(
        readme.contains("acl grant") && readme.contains("only"),
        "the single-writer guarantee (D32) must be documented"
    );
}

#[test]
fn development_docs_explain_running_against_the_mock_issuer() {
    let development = development();
    assert!(development.contains("MDSHELF_OIDC_DISCOVERY_URL"));
    assert!(development.contains("mock_idp.rs"));
    assert!(
        development.contains("leak_suite"),
        "contributors should be pointed at the test that matters most"
    );
    assert!(
        development.contains("Invariants to preserve"),
        "the invariants belong where someone changing the code will see them"
    );
}

/// The worked example has to demonstrate all three levels, including the `deny` that a
/// folder needs to exclude somebody a broader rule already granted — the part of D6
/// that is easiest to get wrong.
#[test]
fn the_example_vault_demonstrates_all_three_rule_levels() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/private-vault");

    let site = std::fs::read_to_string(format!("{root}/index.md")).expect("site index");
    assert!(site.contains("allow:"), "site-level rule missing");

    let folder = std::fs::read_to_string(format!("{root}/hr/index.md")).expect("folder index");
    assert!(folder.contains("allow:"), "folder-level rule missing");
    assert!(
        folder.contains("deny:"),
        "the example must show the deny a folder needs to narrow an inherited grant"
    );

    let file = std::fs::read_to_string(format!("{root}/hr/compensation.md")).expect("file rule");
    assert!(file.contains("deny:"), "file-level rule missing");
}

/// The example vault must itself be valid, or the first thing a new user copies is a
/// vault that `mdshelf check` rejects.
#[test]
fn the_example_vault_passes_mdshelf_check() {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = dir.path().join("mdshelf.toml");
    std::fs::write(
        &config,
        format!(
            "[[sites]]\npath = \"{}/examples/private-vault\"\nmount = \"/handbook\"\ntitle = \"Handbook\"\n",
            env!("CARGO_MANIFEST_DIR")
        ),
    )
    .expect("config");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mdshelf"))
        .args(["check", "--config", config.to_str().expect("path")])
        .output()
        .expect("running check");

    assert!(
        output.status.success(),
        "the documented example vault must validate:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("access rules:"),
        "the example should actually declare rules"
    );
}
