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

// ---------------------------------------------------------------------------
// US-18: the sharing documentation, and the claims it makes about behaviour
// ---------------------------------------------------------------------------

fn help_for(args: &[&str]) -> String {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mdshelf"))
        .args(args)
        .arg("--help")
        .output()
        .expect("running the mdshelf binary");
    assert!(output.status.success(), "{args:?} --help failed");
    flattened(&String::from_utf8_lossy(&output.stdout))
}

#[test]
fn the_readme_documents_the_control_the_cli_and_the_links_section() {
    let readme = readme();
    assert!(
        readme.contains("Share** control") || readme.contains("**Share** control"),
        "the button a reader will look for must be described"
    );
    for command in [
        "mdshelf share /docs/hr/comp",
        "mdshelf share list",
        "mdshelf share revoke",
    ] {
        assert!(readme.contains(command), "{command} is undocumented");
    }
    for key in [
        "[links]",
        "default_lifetime",
        "max_lifetime",
        "revoked_retention",
        "bad_link_retention",
        "prefix",
    ] {
        assert!(readme.contains(key), "{key} is undocumented");
    }
    assert!(
        readme.contains("What a share link is not"),
        "the out-of-scope list is the part that stops support questions"
    );
    assert!(
        readme.contains("shown once and cannot be recovered"),
        "S5's consequence must be stated where somebody will read it"
    );
}

/// US-18: the four things the security note has to say.
#[test]
fn the_readme_security_note_states_what_a_bearer_link_costs() {
    let readme = readme();
    assert!(
        readme.contains("bearer credential"),
        "a link is a bearer credential, and that word matters"
    );
    assert!(
        readme.contains("audit names a link, not a person"),
        "the audit's honesty about who read a page must be stated"
    );
    assert!(
        readme.contains("banner exposes the sharer's address"),
        "R1: the sharer's address reaches everyone the URL reaches"
    );
    assert!(
        readme.contains("tracks its issuer's live access"),
        "S29 must be documented, including that it can break a working link"
    );
    assert!(
        readme.contains("link-preview") || readme.contains("Link-preview"),
        "R4 belongs in the note too"
    );
}

/// R3/NFR-3 amended: the database is no longer disposable, and the docs must not say
/// otherwise.
#[test]
fn the_readme_no_longer_claims_the_database_is_disposable() {
    let readme = readme();
    assert!(
        !readme.contains("deleting it costs only live sessions and access history"),
        "share links cannot be recreated, so this claim is now false"
    );
    assert!(
        readme.contains("not disposable") || readme.contains("is not disposable"),
        "the reversal has to be stated, not merely implied by its absence"
    );
    assert!(
        readme.contains("share list --json"),
        "the inventory that survives a deletion should be named"
    );
}

/// US-18: `{{ share_control }}` is a theme capability, and omitting it has a stated
/// consequence.
#[test]
fn the_readme_documents_the_share_control_as_a_theme_capability() {
    let readme = readme();
    assert!(readme.contains("{{ share_control }}"), "the variable name");
    assert!(
        readme.contains("has no sharing"),
        "the consequence of omitting it must be explicit (R5)"
    );
    assert!(
        readme.contains("layouts/link.html"),
        "the reading-view template is the other theme capability"
    );
}

/// The README's stated defaults are checked against the code, not just against
/// themselves — this is the half of a doc guard that catches real drift.
#[test]
fn the_readme_states_the_defaults_the_code_actually_uses() {
    let defaults = mdshelf::config::LinksConfig::default();
    let readme = readme();
    for (key, value) in [
        ("prefix", defaults.prefix.as_str()),
        ("default_lifetime", defaults.default_lifetime.as_str()),
        ("max_lifetime", defaults.max_lifetime.as_str()),
        ("revoked_retention", defaults.revoked_retention.as_str()),
    ] {
        let quoted = format!("\"{value}\"");
        assert!(
            readme.contains(&format!("{key} = {quoted}"))
                || readme.contains(&format!("{key}  = {quoted}"))
                || readme.contains(&format!("{key}   = {quoted}"))
                || readme.contains(&format!("{key}    = {quoted}"))
                || readme.contains(&format!("{key}     = {quoted}"))
                || readme.contains(&format!("{key}  {quoted}")),
            "the README's {key} does not match the code's default of {quoted}"
        );
    }
    assert!(
        defaults.enabled,
        "the README describes `enabled` as a kill switch that is on by default"
    );
    assert!(
        readme.contains("/s/") || readme.contains(&defaults.prefix),
        "the example URL should use the default prefix"
    );
}

/// US-18: `--help` for `share` and each subcommand.
#[test]
fn share_help_documents_the_flags_and_that_the_url_is_shown_once() {
    let share = help_for(&["share"]);
    for flag in ["--for", "--until", "--base-url"] {
        assert!(share.contains(flag), "share --help omits {flag}");
    }
    assert!(
        share.contains("cannot be recovered"),
        "share --help must say the URL is shown once"
    );

    let list = help_for(&["share", "list"]);
    assert!(list.contains("cannot be recovered"), "got: {list}");
    assert!(list.contains("--all") && list.contains("--json"));

    let revoke = help_for(&["share", "revoke"]);
    assert!(revoke.contains("--all"));
    assert!(
        revoke.contains("not recoverable") || revoke.contains("cannot be reinstated"),
        "revoke --help should say a revoked link cannot come back: {revoke}"
    );
}

// ---------------------------------------------------------------------------
// §11.4: copy / download as Markdown
// ---------------------------------------------------------------------------

/// D2's consequence is the one a reader will test by pasting: no YAML comes with the
/// text. If that ever stops being true it is an ACL leak, so the claim is pinned here.
#[test]
fn the_readme_states_that_frontmatter_is_not_included() {
    let readme = readme();
    assert!(
        readme.contains("Frontmatter is not included"),
        "the docs must say plainly that the copied and downloaded Markdown has no YAML"
    );
    assert!(
        readme.contains("no `allow` or `deny` list"),
        "and that this is what keeps the rule keys out of it"
    );
    assert!(
        readme.contains("Copy as Markdown") && readme.contains("Download Markdown"),
        "both actions must be documented by the names the UI uses"
    );
}

/// D13's consequence on the product's flagship reading path: a tailnet address is plain
/// HTTP, so the clipboard API is simply absent there. A reader who hits the fallback
/// needs to find it described rather than assume the feature is broken.
#[test]
fn the_readme_explains_the_plain_http_clipboard_fallback() {
    let readme = readme();
    assert!(
        readme.contains("Clipboard access is blocked over plain HTTP"),
        "the limitation must be stated, not discovered"
    );
    assert!(
        readme.contains("secure context"),
        "the reason belongs with the symptom"
    );
    assert!(
        readme.contains("Select and copy"),
        "the manual-selection fallback must be named as the UI names it"
    );
}
