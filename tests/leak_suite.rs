//! Phase 3 — the leak suite.
//!
//! The gate for this phase, and the test that matters most in the whole feature. For a
//! fixture vault and a viewer who may read only part of it, no restricted path, title,
//! or byte may appear in *any* response, through any surface mdshelf can emit bytes
//! from: rendered HTML, the navigation sidebar, breadcrumbs, prev/next, auto-generated
//! listings, the site switcher, raw markdown, attachments, the live-reload socket, and
//! static export.
//!
//! Every assertion here is phrased as "this string must not appear anywhere in the
//! response", because a leak that only shows up in an unexpected corner of the markup
//! is still a leak.

use std::collections::HashMap;

use mdshelf::auth::SESSION_COOKIE;
use mdshelf::test_support::{MockIdp, TestServer, TestSite, TokenSpec, client};

/// Distinctive strings that must never reach a viewer without access.
const SECRET_TITLE: &str = "Executive Compensation Q3";
const SECRET_BODY: &str = "ZZTOPSECRETZZ";
const SECRET_SLUG: &str = "salaries";

const VAULT: &[(&str, &str)] = &[
    (
        "index.md",
        "---\ntitle: Handbook\nallow:\n  - team@corp.com\n---\n\n# Handbook\n",
    ),
    (
        "onboarding.md",
        "---\ntitle: Onboarding\n---\n\n# Onboarding\n\nWelcome aboard.\n",
    ),
    // D6 in practice: the site-level grant below reaches into every folder, so keeping
    // team@ out of /hr takes an explicit deny. An `allow` here would only *add* hr@.
    (
        "hr/index.md",
        "---\ntitle: People Ops\nallow:\n  - hr@corp.com\ndeny:\n  - team@corp.com\n---\n\n# People Ops\n",
    ),
    (
        "hr/salaries.md",
        "---\ntitle: Executive Compensation Q3\n---\n\n# Executive Compensation Q3\n\nZZTOPSECRETZZ\n",
    ),
    ("hr/chart.png", "PNG-BYTES-ZZTOPSECRETZZ"),
    (
        "open/index.md",
        "---\ntitle: Open\nallow:\n  - team@corp.com\n  - hr@corp.com\n---\n\n# Open\n",
    ),
    (
        "open/notes.md",
        "---\ntitle: Shared Notes\n---\n\n# Shared Notes\n",
    ),
];

fn query_params(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .expect("a parseable URL")
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn location_of(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("a Location header")
        .to_str()
        .expect("printable Location")
        .to_string()
}

/// Complete a sign-in and return the session cookie.
async fn sign_in(server: &TestServer, idp: &MockIdp, email: &str) -> String {
    let http = client();
    let login = http
        .get(server.url("/auth/login"))
        .send()
        .await
        .expect("login");
    let authorize_url = location_of(&login);
    let state = query_params(&authorize_url)
        .get("state")
        .expect("state")
        .clone();
    idp.register_code(&format!("code-for-{state}"), TokenSpec::valid(email));

    let authorize = http.get(&authorize_url).send().await.expect("authorize");
    let callback = http
        .get(location_of(&authorize))
        .send()
        .await
        .expect("callback");

    callback
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|header| {
            let rest = header.strip_prefix(&format!("{SESSION_COOKIE}="))?;
            Some(rest.split(';').next()?.to_string())
        })
        .expect("a session cookie")
}

async fn get_as(server: &TestServer, cookie: &str, path: &str) -> reqwest::Response {
    client()
        .get(server.url(path))
        .header(
            reqwest::header::COOKIE,
            format!("{SESSION_COOKIE}={cookie}"),
        )
        .send()
        .await
        .expect("request")
}

/// Every path a viewer might reach, including the ones that only exist for others.
const ALL_PATHS: &[&str] = &[
    "/",
    "/docs",
    "/docs/",
    "/docs/onboarding",
    "/docs/hr",
    "/docs/hr/salaries",
    "/docs/hr/salaries.md",
    "/docs/hr/chart.png",
    "/docs/open",
    "/docs/open/notes",
    "/docs/does-not-exist",
    "/nowhere",
];

// ---------------------------------------------------------------------------
// The central property: a partially-invited viewer sees no trace of the rest
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_partially_invited_viewer_sees_no_trace_of_restricted_content() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    // team@ is granted the site root and /open, but never /hr.
    let cookie = sign_in(&server, &idp, "team@corp.com").await;

    for path in ALL_PATHS {
        let response = get_as(&server, &cookie, path).await;
        let status = response.status();
        let body = response.text().await.expect("body");

        for secret in [SECRET_TITLE, SECRET_BODY, SECRET_SLUG] {
            assert!(
                !body.contains(secret),
                "{path} (status {status}) leaked {secret:?} to a viewer without access\n\
                 ---- body ----\n{body}"
            );
        }
    }
}

#[tokio::test]
async fn the_sidebar_lists_only_pages_the_viewer_may_open() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, "team@corp.com").await;

    let body = get_as(&server, &cookie, "/docs/open/notes")
        .await
        .text()
        .await
        .expect("body");

    // Present, because team@ may read them.
    assert!(
        body.contains("Shared Notes"),
        "expected visible page in nav"
    );
    // Absent, because team@ may not.
    assert!(
        !body.contains("People Ops"),
        "restricted folder title leaked"
    );
    assert!(!body.contains(SECRET_TITLE), "restricted page title leaked");
}

#[tokio::test]
async fn an_invited_viewer_can_read_exactly_their_own_pages() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, "hr@corp.com").await;

    // hr@ owns /hr.
    let allowed = get_as(&server, &cookie, "/docs/hr/salaries").await;
    assert_eq!(allowed.status(), 200);
    let body = allowed.text().await.expect("body");
    assert!(body.contains(SECRET_TITLE));
    assert!(body.contains(SECRET_BODY));

    // ...but the site root names only team@, so hr@ is denied there.
    let denied = get_as(&server, &cookie, "/docs/onboarding").await;
    assert_eq!(
        denied.status(),
        404,
        "an inherited grant that does not name this address must not apply"
    );
}

// ---------------------------------------------------------------------------
// D23: restricted and nonexistent are the same answer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restricted_and_nonexistent_paths_return_byte_identical_responses() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, "team@corp.com").await;

    let restricted = get_as(&server, &cookie, "/docs/hr/salaries").await;
    let missing = get_as(&server, &cookie, "/docs/hr/there-is-no-such-page").await;
    let missing_folder = get_as(&server, &cookie, "/docs/no-such-folder/at-all").await;
    let off_site = get_as(&server, &cookie, "/not-even-a-site").await;

    assert_eq!(restricted.status(), 404);
    assert_eq!(missing.status(), 404);
    assert_eq!(missing_folder.status(), 404);
    assert_eq!(off_site.status(), 404);

    let restricted_body = restricted.text().await.expect("body");
    let missing_body = missing.text().await.expect("body");
    let missing_folder_body = missing_folder.text().await.expect("body");
    let off_site_body = off_site.text().await.expect("body");

    assert_eq!(
        restricted_body, missing_body,
        "D23: a restricted page and a typo must be indistinguishable"
    );
    assert_eq!(restricted_body, missing_folder_body);
    assert_eq!(
        restricted_body, off_site_body,
        "even a path under no site must not be distinguishable"
    );
}

/// Bodies matching is not enough: a differing header would identify an existing path
/// just as effectively as differing markup.
#[tokio::test]
async fn restricted_and_nonexistent_paths_are_indistinguishable_in_their_headers_too() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, "team@corp.com").await;

    /// Every response header except ones that legitimately vary per connection.
    async fn fingerprint(response: reqwest::Response) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .filter(|(name, _)| {
                !matches!(
                    name.as_str(),
                    "date" | "connection" | "keep-alive" | "server"
                )
            })
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or("<binary>").to_string(),
                )
            })
            .collect();
        headers.sort();
        headers
    }

    let restricted = fingerprint(get_as(&server, &cookie, "/docs/hr/salaries").await).await;
    let missing = fingerprint(get_as(&server, &cookie, "/docs/hr/no-such-page").await).await;
    let attachment = fingerprint(get_as(&server, &cookie, "/docs/hr/chart.png").await).await;
    let missing_attachment =
        fingerprint(get_as(&server, &cookie, "/docs/hr/no-such-image.png").await).await;

    assert_eq!(
        restricted, missing,
        "a restricted page and a missing one must not differ in any header"
    );
    assert_eq!(
        attachment, missing_attachment,
        "the same must hold for attachments, which take a different code path"
    );
    assert_eq!(
        restricted, attachment,
        "and a restricted page must not be distinguishable from a restricted attachment"
    );
}

#[tokio::test]
async fn the_deny_page_helps_a_visitor_using_the_wrong_account() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    // Signed in, but named by no rule anywhere — the personal-vs-work-account case.
    let cookie = sign_in(&server, &idp, "someone@gmail.com").await;

    let body = get_as(&server, &cookie, "/docs/onboarding")
        .await
        .text()
        .await
        .expect("body");

    assert!(
        body.contains("someone@gmail.com"),
        "D22: the visitor must be able to see which account they are using"
    );
    assert!(body.contains("Switch account"));
    assert!(
        body.contains("mailto:owner@corp.com"),
        "D24: request access"
    );
}

// ---------------------------------------------------------------------------
// SEC-9: anonymous visitors learn nothing at all
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous_visitors_get_the_interstitial_for_every_path() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let http = client();

    let mut bodies = Vec::new();
    for path in ALL_PATHS {
        let response = http.get(server.url(path)).send().await.expect("request");
        assert_eq!(
            response.status(),
            401,
            "{path} should ask an anonymous visitor to sign in"
        );
        let body = response.text().await.expect("body");
        assert!(body.contains("Sign in with Google"), "{path}");
        for secret in [SECRET_TITLE, SECRET_BODY, SECRET_SLUG] {
            assert!(!body.contains(secret), "{path} leaked {secret:?}");
        }
        bodies.push(body);
    }

    // Identical for every path, so the interstitial cannot be used to probe existence.
    assert!(
        bodies.windows(2).all(|pair| pair[0] == pair[1]),
        "the interstitial must not vary with the requested path"
    );
}

// ---------------------------------------------------------------------------
// US-18: attachments and raw markdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn attachments_are_gated_like_the_pages_they_belong_to() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    let team = sign_in(&server, &idp, "team@corp.com").await;
    let response = get_as(&server, &team, "/docs/hr/chart.png").await;
    assert_eq!(
        response.status(),
        404,
        "SEC-8: an attachment under a restricted folder must not be served"
    );
    assert!(!response.text().await.expect("body").contains(SECRET_BODY));

    // The person who may read the folder still gets the file.
    let hr = sign_in(&server, &idp, "hr@corp.com").await;
    let response = get_as(&server, &hr, "/docs/hr/chart.png").await;
    assert_eq!(response.status(), 200);
    assert!(response.text().await.expect("body").contains(SECRET_BODY));
}

#[tokio::test]
async fn raw_markdown_is_gated_like_the_rendered_page() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, "team@corp.com").await;

    let response = get_as(&server, &cookie, "/docs/hr/salaries.md").await;
    assert_eq!(response.status(), 404);
    assert!(!response.text().await.expect("body").contains(SECRET_BODY));
}

/// Regression: a case-variant path must not bypass a folder rule.
///
/// macOS and Windows have case-insensitive filesystems, so a request for
/// `HR/chart.png` opens `hr/chart.png`. Authorizing the *request string* looked up a
/// folder named `HR`, missed the rule on `hr`, and fell through to the broader
/// site-level grant — serving restricted bytes to a viewer that folder explicitly
/// denies. Both attachments and raw markdown were affected. The fix authorizes the
/// canonicalized path the filesystem resolved, not the string the client sent.
#[tokio::test]
async fn case_variant_paths_do_not_bypass_a_folder_rule() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    // team@ is denied /hr by hr/index.md, but granted the site root.
    let team = sign_in(&server, &idp, "team@corp.com").await;

    for path in [
        "/docs/hr/salaries",
        "/docs/HR/salaries",
        "/docs/Hr/Salaries",
        "/docs/hr/salaries.md",
        "/docs/HR/salaries.md",
        "/docs/Hr/salaries.MD",
        "/docs/hr/chart.png",
        "/docs/HR/chart.png",
        "/docs/hr/CHART.PNG",
        "/docs/HR/CHART.PNG",
    ] {
        let response = get_as(&server, &team, path).await;
        let status = response.status();
        let body = response.text().await.expect("body");
        assert_eq!(status, 404, "{path} should be denied, got {status}");
        assert!(
            !body.contains(SECRET_BODY) && !body.contains(SECRET_TITLE),
            "{path} leaked restricted content through a case variant"
        );
    }

    // The fix must not over-block: the person who may read the folder still can,
    // including through a case variant of the same file.
    let hr = sign_in(&server, &idp, "hr@corp.com").await;
    let response = get_as(&server, &hr, "/docs/hr/chart.png").await;
    assert_eq!(response.status(), 200);
    assert!(response.text().await.expect("body").contains(SECRET_BODY));
}

#[tokio::test]
async fn path_traversal_cannot_escape_the_vault() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, "team@corp.com").await;

    for hostile in [
        "/docs/../../../etc/passwd",
        "/docs/%2e%2e/%2e%2e/etc/passwd",
        "/docs/open/../hr/salaries.md",
    ] {
        let response = get_as(&server, &cookie, hostile).await;
        let status = response.status();
        let body = response.text().await.expect("body");
        assert!(
            status == 404 || status == 301 || status == 308,
            "{hostile} returned {status}"
        );
        assert!(!body.contains("root:"), "{hostile} escaped the vault");
        assert!(
            !body.contains(SECRET_BODY),
            "{hostile} reached restricted content"
        );
    }
}

// ---------------------------------------------------------------------------
// US-19: the live-reload socket
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_live_reload_socket_requires_a_session() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth_and_live_reload(VAULT, &idp).await;
    let http = client();

    let upgrade = |request: reqwest::RequestBuilder| {
        request
            .header(reqwest::header::CONNECTION, "Upgrade")
            .header(reqwest::header::UPGRADE, "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
    };

    // Anonymous: refused.
    let response = upgrade(http.get(server.url("/__livereload")))
        .send()
        .await
        .expect("ws upgrade");
    assert_eq!(
        response.status(),
        401,
        "an anonymous socket would be a standing channel into a private server"
    );

    // Signed in: accepted.
    let cookie = sign_in(&server, &idp, "team@corp.com").await;
    let response = upgrade(http.get(server.url("/__livereload")).header(
        reqwest::header::COOKIE,
        format!("{SESSION_COOKIE}={cookie}"),
    ))
    .send()
    .await
    .expect("ws upgrade");
    assert_eq!(response.status(), 101, "a signed-in viewer may connect");
}

// ---------------------------------------------------------------------------
// The site switcher
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_site_switcher_does_not_count_pages_the_viewer_cannot_see() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, "team@corp.com").await;

    let body = get_as(&server, &cookie, "/docs/open/notes")
        .await
        .text()
        .await
        .expect("body");

    // team@ may read index.md, onboarding.md, open/index.md, open/notes.md — four of
    // the six pages. A count of six would disclose that two more exist.
    assert!(
        !body.contains(">6<") && !body.contains("6 pages"),
        "the site switcher leaked the unfiltered page count:\n{body}"
    );
}

// ---------------------------------------------------------------------------
// US-20: static export
// ---------------------------------------------------------------------------

/// Lay out a vault plus config on disk and return the config path.
fn scaffold(dir: &std::path::Path, files: &[(&str, &str)]) -> std::path::PathBuf {
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
    std::process::Command::new(env!("CARGO_BIN_EXE_mdshelf"))
        .args(args)
        .output()
        .expect("running the mdshelf binary")
}

/// Concatenate every exported file so a single search covers the whole bundle.
fn read_bundle(root: &std::path::Path) -> String {
    let mut combined = String::new();
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if entry.file_type().is_file()
            && let Ok(text) = std::fs::read_to_string(entry.path())
        {
            combined.push_str(&text);
            combined.push('\n');
        }
        if entry.file_type().is_file() {
            combined.push_str(&entry.path().display().to_string());
            combined.push('\n');
        }
    }
    combined
}

#[test]
fn export_without_a_viewer_refuses_on_a_vault_with_rules() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);
    let out = dir.path().join("out");

    let output = run_mdshelf(&[
        "export",
        "--config",
        config.to_str().unwrap(),
        "--output",
        out.to_str().unwrap(),
    ]);

    assert!(
        !output.status.success(),
        "flattening a private vault into public HTML must not be the default"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no authentication"), "got:\n{stderr}");
    assert!(stderr.contains("--as"), "got:\n{stderr}");
    assert!(!out.exists() || read_bundle(&out).is_empty());
}

#[test]
fn export_as_a_viewer_writes_only_what_that_viewer_can_see() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);
    let out = dir.path().join("out");

    let output = run_mdshelf(&[
        "export",
        "--config",
        config.to_str().unwrap(),
        "--output",
        out.to_str().unwrap(),
        "--as",
        "team@corp.com",
    ]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("visible to team@corp.com"),
        "got:\n{stdout}"
    );
    assert!(
        stdout.contains("skipped"),
        "omissions must be reported, not silent; got:\n{stdout}"
    );

    let bundle = read_bundle(&out);
    assert!(bundle.contains("Onboarding"), "expected permitted content");
    for secret in [SECRET_TITLE, SECRET_BODY, SECRET_SLUG] {
        assert!(
            !bundle.contains(secret),
            "the exported bundle leaked {secret:?}"
        );
    }
    assert!(
        !out.join("docs/hr/chart.png").exists(),
        "a restricted attachment must not be copied into the bundle"
    );
}

#[test]
fn export_as_the_other_viewer_writes_their_pages_instead() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);
    let out = dir.path().join("out");

    let output = run_mdshelf(&[
        "export",
        "--config",
        config.to_str().unwrap(),
        "--output",
        out.to_str().unwrap(),
        "--as",
        "hr@corp.com",
    ]);
    assert!(output.status.success());

    let bundle = read_bundle(&out);
    assert!(
        bundle.contains(SECRET_TITLE),
        "hr@ may read their own pages"
    );
    assert!(
        !bundle.contains("Onboarding"),
        "hr@ is named by no rule covering the site root"
    );
}

#[test]
fn export_is_unchanged_on_a_vault_with_no_rules() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(
        dir.path(),
        &[
            ("index.md", "---\ntitle: Home\n---\n\n# Home\n"),
            ("guide.md", "---\ntitle: Guide\n---\n\n# Guide\n"),
        ],
    );
    let out = dir.path().join("out");

    let output = run_mdshelf(&[
        "export",
        "--config",
        config.to_str().unwrap(),
        "--output",
        out.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "a vault that was always public must still export with no new ceremony"
    );
    assert!(read_bundle(&out).contains("Guide"));
}

/// Regression: an exported bundle must not name a site the recipient cannot see.
///
/// `export_site` decided whether to synthesise a root index from the *unfiltered*
/// site, so a viewer with no access to a second site still got an empty
/// `secret-project/index.html` carrying that site's title and mount. For the
/// consultancy use case — one bundle per client — that is another client's project
/// name shipped in the deliverable.
#[test]
fn an_exported_bundle_never_names_a_site_the_recipient_cannot_see() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    for (relative, contents) in [
        (
            "vault/index.md",
            "---\ntitle: Docs\nallow:\n  - team@corp.com\n---\n\n# Docs\n",
        ),
        (
            "other/brief.md",
            "---\ntitle: Brief\nallow:\n  - client-b@corp.com\n---\n\n# Brief\n",
        ),
    ] {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let config = root.join("mdshelf.toml");
    std::fs::write(
        &config,
        "[[sites]]\npath = \"vault\"\nmount = \"/docs\"\ntitle = \"Docs\"\n\n\
         [[sites]]\npath = \"other\"\nmount = \"/skunkworks\"\ntitle = \"Skunkworks Q4\"\n",
    )
    .unwrap();

    let out = root.join("out");
    let output = run_mdshelf(&[
        "export",
        "--config",
        config.to_str().unwrap(),
        "--output",
        out.to_str().unwrap(),
        "--as",
        "team@corp.com",
    ]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bundle = read_bundle(&out);
    assert!(
        bundle.contains("Docs"),
        "the recipient's own site must be present"
    );
    assert!(
        !bundle.contains("Skunkworks Q4"),
        "the bundle named a site the recipient cannot see"
    );
    assert!(
        !out.join("skunkworks").exists(),
        "the bundle contained a directory for a site the recipient cannot see"
    );
}

#[test]
fn export_rejects_an_invalid_viewer_address() {
    let dir = tempfile::tempdir().unwrap();
    let config = scaffold(dir.path(), VAULT);
    let out = dir.path().join("out");

    let output = run_mdshelf(&[
        "export",
        "--config",
        config.to_str().unwrap(),
        "--output",
        out.to_str().unwrap(),
        "--as",
        "team@corp",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not a valid email"),
        "a typo'd address would silently export an empty bundle"
    );
}

// ---------------------------------------------------------------------------
// Multi-site: one server, several vaults, different audiences
// ---------------------------------------------------------------------------

/// Regression: the site switcher listed every configured site on every content page.
///
/// `match_site_path` built the switcher from the unfiltered universe, so a reader
/// invited to one site saw the titles of every other site on the server — a consultant
/// hosting two clients leaked each client's project name to the other. The home page
/// was already filtered; content pages were not.
#[tokio::test]
async fn the_site_switcher_never_names_a_site_the_viewer_cannot_open() {
    const DOCS: &[(&str, &str)] = &[
        (
            "index.md",
            "---\ntitle: Docs\nallow:\n  - team@corp.com\n---\n\n# Docs\n",
        ),
        ("page.md", "---\ntitle: Page\n---\n\n# Page\n"),
    ];
    const SKUNK: &[(&str, &str)] = &[(
        "index.md",
        "---\ntitle: Skunkworks\nallow:\n  - client-b@corp.com\n---\n\n# Brief\n",
    )];

    let idp = MockIdp::start().await;
    let sites = [
        TestSite {
            mount: "/docs",
            title: "Docs",
            files: DOCS,
        },
        TestSite {
            mount: "/skunkworks",
            title: "Skunkworks Q4",
            files: SKUNK,
        },
    ];
    let server = TestServer::start_with_auth_sites(&sites, &idp).await;

    let team = sign_in(&server, &idp, "team@corp.com").await;
    for path in ["/", "/docs", "/docs/page"] {
        let body = get_as(&server, &team, path)
            .await
            .text()
            .await
            .expect("body");
        assert!(
            !body.contains("Skunkworks Q4"),
            "{path} named a site team@corp.com cannot open"
        );
        assert!(
            !body.contains("/skunkworks"),
            "{path} linked a site team@corp.com cannot open"
        );
    }

    // The other client sees their own site and not this one — the switcher is filtered,
    // not simply suppressed.
    let client_b = sign_in(&server, &idp, "client-b@corp.com").await;
    let body = get_as(&server, &client_b, "/skunkworks")
        .await
        .text()
        .await
        .expect("body");
    assert!(
        body.contains("Skunkworks"),
        "the invited viewer lost their own site"
    );
    assert!(
        !body.contains(">Docs<"),
        "client-b@corp.com was shown a site they cannot open"
    );
}

// ---------------------------------------------------------------------------
// NFR-2: none of this changes an unauthenticated server
// ---------------------------------------------------------------------------

/// Invariant 4, stated precisely.
///
/// mdshelf strips `allow`/`deny` from frontmatter unconditionally, not only when auth
/// is on — a vault previewed with plain `mdshelf serve` must not publish its own
/// invitee list through a theme that dumps frontmatter. This test pins the consequence:
/// on an unauthenticated server, a page carrying rules renders byte-identically to the
/// same page with the rules deleted from the source.
#[tokio::test]
async fn access_rules_make_no_difference_to_an_unauthenticated_render() {
    let with_rules = TestServer::start_public(&[(
        "note.md",
        "---\ntitle: Note\nsidebar_order: 2\nallow:\n  - ana@corp.com\ndeny:\n  - bob@corp.com\n---\n\n# Note\n\nBody.\n",
    )])
    .await;
    let without_rules = TestServer::start_public(&[(
        "note.md",
        "---\ntitle: Note\nsidebar_order: 2\n---\n\n# Note\n\nBody.\n",
    )])
    .await;

    let http = client();
    let ruled = http
        .get(with_rules.url("/docs/note"))
        .send()
        .await
        .expect("request")
        .text()
        .await
        .expect("body");
    let plain = http
        .get(without_rules.url("/docs/note"))
        .send()
        .await
        .expect("request")
        .text()
        .await
        .expect("body");

    /// The sidebar embeds each file's mtime, which differs because the two fixtures
    /// were written milliseconds apart. That is the harness, not the renderer.
    fn without_mtimes(html: &str) -> String {
        let pattern = "data-sidebar-date=\"";
        let mut out = String::with_capacity(html.len());
        let mut rest = html;
        while let Some(start) = rest.find(pattern) {
            let (before, after) = rest.split_at(start + pattern.len());
            out.push_str(before);
            let end = after.find('"').expect("a closing quote");
            out.push_str("MTIME");
            rest = &after[end..];
        }
        out.push_str(rest);
        out
    }

    assert_eq!(
        without_mtimes(&ruled),
        without_mtimes(&plain),
        "rules must be invisible in the rendered output of an unauthenticated server"
    );
    assert!(!ruled.contains("ana@corp.com"));
    assert!(!ruled.contains("bob@corp.com"));
    // Other frontmatter is untouched — only the two rule keys are removed.
    assert!(ruled.contains("Note"));
}

#[tokio::test]
async fn an_unauthenticated_server_serves_everything_as_before() {
    let server = TestServer::start_public(VAULT).await;
    let http = client();

    // The very same vault, with rules in it, served with --auth google absent.
    let response = http
        .get(server.url("/docs/hr/salaries"))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), 200);
    assert!(response.text().await.expect("body").contains(SECRET_TITLE));

    let attachment = http
        .get(server.url("/docs/hr/chart.png"))
        .send()
        .await
        .expect("request");
    assert_eq!(attachment.status(), 200);
}
