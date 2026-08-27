//! Phase 2 — serving a share link (US-7 … US-13), plus the test surfaces from §11.
//!
//! Every assertion here is about what a recipient with no account receives. The two
//! that matter most are the pair at the bottom: a link reaches exactly one page and its
//! referenced assets (SEC-4), and every way of failing produces the same bytes (SEC-3).

use std::collections::HashMap;

use mdshelf::auth::SESSION_COOKIE;
use mdshelf::content::page::visit_attribute_urls;
use mdshelf::test_support::{MockIdp, TestServer, TokenSpec, client};

const ISSUER: &str = "ana@corp.com";
const OTHER_INVITEE: &str = "bob@corp.com";
const PAGE_BODY: &str = "Salary bands for the current year.";
const OTHER_PAGE_BODY: &str = "ZZ-OTHER-PAGE-ZZ";

const VAULT: &[(&str, &str)] = &[
    (
        "index.md",
        "---\ntitle: Handbook\nallow:\n  - ana@corp.com\n  - bob@corp.com\n---\n\n# Handbook\n",
    ),
    (
        "hr/comp.md",
        "---\ntitle: Compensation\n---\n\n# Compensation\n\n\
         ![Chart](../img/chart.png)\n\n[Policy](policy.md)\n\n\
         Salary bands for the current year.\n",
    ),
    (
        "hr/policy.md",
        "---\ntitle: Policy\n---\n\n# Policy\n\nZZ-OTHER-PAGE-ZZ\n",
    ),
    ("hr/secret.pdf", "PDF-NEVER-REFERENCED"),
    ("img/chart.png", "PNG-CHART-BYTES"),
];

const SHARED_PAGE: &str = "hr/comp.md";
const ONE_HOUR: i64 = 3_600_000;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

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

async fn get(server: &TestServer, path: &str) -> reqwest::Response {
    client()
        .get(server.url(path))
        .send()
        .await
        .expect("request")
}

/// Status, body and the headers a link response is judged on — the whole observable
/// answer, so "byte-identical" means byte-identical.
async fn fingerprint(response: reqwest::Response) -> (u16, Vec<(String, String)>, String) {
    let status = response.status().as_u16();
    let mut headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .filter(|(name, _)| {
            // Date and connection bookkeeping vary between two responses of any kind.
            !matches!(name.as_str(), "date" | "connection" | "keep-alive")
        })
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    headers.sort();
    let body = response.text().await.expect("body");
    (status, headers, body)
}

/// Every root-relative URL the rendered page points at, ignoring mdshelf's own routes.
fn in_vault_urls(html: &str) -> Vec<String> {
    let mut found = Vec::new();
    visit_attribute_urls(html, |url| {
        if url.starts_with('/') && !url.starts_with("//") && !url.starts_with("/__") {
            found.push(url.to_string());
        }
    });
    found
}

async fn start(files: &[(&str, &str)], idp: &MockIdp) -> TestServer {
    TestServer::start_with_auth(files, idp).await
}

// ---------------------------------------------------------------------------
// US-7 — a valid link serves its page
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_valid_link_serves_its_page_in_a_reading_view() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);

    let response = get(&server, &format!("/s/{token}")).await;
    assert_eq!(response.status(), 200);

    let headers = response.headers().clone();
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
    assert_eq!(headers["cache-control"], "no-store");

    let body = response.text().await.expect("body");
    assert!(body.contains(PAGE_BODY), "the page content must be served");

    // The reading view: none of the chrome a signed-in reader gets. The precondition
    // for each absence is asserted below against the same page, signed in.
    for chrome in [
        "doc-sidebar",
        "doc-prev-next",
        "partials/breadcrumbs",
        "doc-breadcrumbs",
        "site-switcher",
        "Handbook",
        "Docs",
    ] {
        assert!(
            !body.contains(chrome),
            "the reading view must not contain {chrome:?}"
        );
    }

    let cookie = sign_in(&server, &idp, ISSUER).await;
    let signed_in = client()
        .get(server.url("/docs/hr/comp"))
        .header(
            reqwest::header::COOKIE,
            format!("{SESSION_COOKIE}={cookie}"),
        )
        .send()
        .await
        .expect("request")
        .text()
        .await
        .expect("body");
    assert!(
        signed_in.contains("doc-sidebar") && signed_in.contains("Docs"),
        "precondition: the signed-in render really does carry the chrome the reading \
         view must drop"
    );
}

#[tokio::test]
async fn the_banner_names_the_issuer_and_the_remaining_time() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, 20 * ONE_HOUR);

    let body = get(&server, &format!("/s/{token}"))
        .await
        .text()
        .await
        .expect("body");
    assert!(body.contains(ISSUER), "S26: the banner names the sharer");
    assert!(
        body.contains("19 hours") || body.contains("20 hours"),
        "the banner should say how long is left: {body}"
    );
}

#[tokio::test]
async fn theme_colours_and_fonts_reach_the_reading_view() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);

    let body = get(&server, &format!("/s/{token}"))
        .await
        .text()
        .await
        .expect("body");
    assert!(body.contains("/__assets/css/main.css"), "theme CSS");
    assert!(
        body.contains("/__assets/vendor/inter/inter.css"),
        "theme font"
    );
    assert!(body.contains("--accent"), "the site's accent colour");

    // The stylesheets a recipient is pointed at must actually be fetchable by someone
    // with no session, or the reading view renders unstyled.
    for asset in ["/__assets/css/main.css", "/__mdshelf/syntax.css"] {
        assert_eq!(get(&server, asset).await.status(), 200, "{asset}");
    }
}

/// SEC-6, extended to the reading view.
#[tokio::test]
async fn no_allow_or_deny_key_reaches_a_link_response() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);

    let body = get(&server, &format!("/s/{token}"))
        .await
        .text()
        .await
        .expect("body");
    assert!(
        std::fs::read_to_string(server.vault.join("index.md"))
            .expect("the fixture")
            .contains(OTHER_INVITEE),
        "precondition: the vault really does name a second invitee"
    );
    assert!(
        !body.contains(OTHER_INVITEE),
        "another invitee's address must never reach a recipient"
    );
    for key in ["allow:", "deny:", "\"allow\"", "\"deny\""] {
        assert!(!body.contains(key), "the {key} key reached the response");
    }
}

// ---------------------------------------------------------------------------
// US-8 — referenced assets, and nothing else
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_referenced_asset_is_reachable_and_nothing_else_is() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);

    // The one asset the page references.
    let asset = get(&server, &format!("/s/{token}/img/chart.png")).await;
    assert_eq!(asset.status(), 200);
    assert_eq!(asset.text().await.expect("body"), "PNG-CHART-BYTES");

    let deny = get(&server, &format!("/s/{token}/nothing-here")).await;
    let (deny_status, _, deny_body) = fingerprint(deny).await;

    for forbidden in [
        // A non-markdown file in the same folder the page never mentions.
        "hr/secret.pdf",
        // Another markdown page in the same folder.
        "hr/policy",
        "hr/policy.md",
        // The raw source of the shared page itself.
        "hr/comp.md",
        // The site root, and a folder listing.
        "",
        "hr",
    ] {
        let response = get(&server, &format!("/s/{token}/{forbidden}")).await;
        let (status, _, body) = fingerprint(response).await;
        assert_eq!(
            status, deny_status,
            "{forbidden} produced a different status"
        );
        assert_eq!(body, deny_body, "{forbidden} produced a different body");
        assert!(
            !body.contains("PDF-NEVER-REFERENCED") && !body.contains(OTHER_PAGE_BODY),
            "{forbidden} leaked content"
        );
    }
}

/// §11 surface 3: the token-completeness test.
#[tokio::test]
async fn every_in_vault_url_in_a_link_response_carries_the_token() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);

    let body = get(&server, &format!("/s/{token}"))
        .await
        .text()
        .await
        .expect("body");
    let urls = in_vault_urls(&body);
    assert!(
        urls.len() >= 2,
        "precondition: the page really does point at in-vault URLs, got {urls:?}"
    );
    let expected_prefix = format!("/s/{token}/");
    for url in &urls {
        assert!(
            url.starts_with(&expected_prefix),
            "{url} is an in-vault URL without the token"
        );
    }
    // And the page's own body really was rewritten, not merely emptied.
    assert!(urls.iter().any(|url| url.ends_with("img/chart.png")));
    assert!(urls.iter().any(|url| url.ends_with("hr/policy")));
}

// ---------------------------------------------------------------------------
// US-9 — dead links
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_dead_unknown_and_malformed_link_answers_identically() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;

    let live = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    let expired = server.mint_link(SHARED_PAGE, ISSUER, -ONE_HOUR);
    let revoked = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    server
        .store()
        .revoke_link(
            &TestServer::link_id(&revoked),
            mdshelf::auth::store::now_ms(),
        )
        .expect("revoking");

    // A well-formed token nobody ever minted.
    let unknown = "AAAAAAAAAAAAAAAAAAAAAA";
    assert_eq!(unknown.len(), 22);

    let candidates = [
        format!("/s/{expired}"),
        format!("/s/{revoked}"),
        format!("/s/{unknown}"),
        "/s/not-a-token".to_string(),
        "/s/".to_string(),
        "/s".to_string(),
        format!("/s/{live}/no-such-path"),
        format!("/s/{unknown}/and/a/tail"),
    ];

    let mut fingerprints = Vec::new();
    for path in &candidates {
        fingerprints.push((path.clone(), fingerprint(get(&server, path).await).await));
    }

    let (first_path, first) = &fingerprints[0];
    for (path, other) in &fingerprints[1..] {
        assert_eq!(other.0, first.0, "{path} and {first_path} differ in status");
        assert_eq!(
            other.1, first.1,
            "{path} and {first_path} differ in headers"
        );
        assert_eq!(other.2, first.2, "{path} and {first_path} differ in body");
    }

    // The precondition that stops this from being an equivalence between two empty
    // sets: a live link is genuinely different.
    let (status, _, body) = fingerprint(get(&server, &format!("/s/{live}")).await).await;
    assert_eq!(status, 200);
    assert_ne!(body, first.2);

    // US-9: the deny body names nothing.
    for leak in [
        "hr/comp",
        "Compensation",
        "Handbook",
        "Docs",
        ISSUER,
        PAGE_BODY,
    ] {
        assert!(!first.2.contains(leak), "the deny page leaked {leak:?}");
    }
}

// ---------------------------------------------------------------------------
// US-10 — issuer authority (S29)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_link_tracks_its_issuers_live_access() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    let url = format!("/s/{token}");

    assert_eq!(
        get(&server, &url).await.status(),
        200,
        "the issuer can read"
    );

    // Remove the issuer from the site's allow list.
    server
        .write_and_rebuild(
            "index.md",
            "---\ntitle: Handbook\nallow:\n  - bob@corp.com\n---\n\n# Handbook\n",
        )
        .await;
    assert_eq!(
        get(&server, &url).await.status(),
        404,
        "S29: the link dies with its issuer's access"
    );

    // Restore it; the link, still unexpired, serves again.
    server
        .write_and_rebuild(
            "index.md",
            "---\ntitle: Handbook\nallow:\n  - ana@corp.com\n  - bob@corp.com\n---\n\n# Handbook\n",
        )
        .await;
    assert_eq!(get(&server, &url).await.status(), 200);

    // D10: a malformed block denies everyone, so the page has no live links at all.
    server
        .write_and_rebuild(
            "index.md",
            "---\ntitle: Handbook\nallow: ana@corp.com\n---\n\n# Handbook\n",
        )
        .await;
    assert_eq!(
        get(&server, &url).await.status(),
        404,
        "a page nobody can read has no live links"
    );
}

/// §11 surface 2: cross-check what a link serves against the resolver's own answer.
#[tokio::test]
async fn what_a_link_serves_matches_the_resolver() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;

    let mut served = 0usize;
    let mut denied = 0usize;
    for issuer in [ISSUER, OTHER_INVITEE, "stranger@corp.com"] {
        for page in ["hr/comp.md", "hr/policy.md", "index.md"] {
            let token = server.mint_link(page, issuer, ONE_HOUR);
            let status = get(&server, &format!("/s/{token}")).await.status();

            // The independently computed answer to "can the issuer read this?".
            let expected = {
                let universe = server.state.universe.read().await;
                let site = universe.sites()[0].clone();
                site.acl().allows(std::path::Path::new(page), issuer)
            };
            if expected {
                served += 1;
                assert_eq!(status, 200, "{issuer} should be able to read {page}");
            } else {
                denied += 1;
                assert_eq!(status, 404, "{issuer} should not be able to read {page}");
            }
        }
    }
    assert!(served > 0 && denied > 0, "both verdicts must be exercised");
}

/// §11 surface 4: lifecycle property test.
///
/// Every reachable combination of expiry, revocation and issuer access, checked against
/// the model "serves exactly when live and the issuer can still read it".
#[tokio::test]
async fn a_link_serves_exactly_when_it_should() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let now = mdshelf::auth::store::now_ms();

    let mut checked = 0usize;
    for expired in [false, true] {
        for revoked in [false, true] {
            for access in [true, false] {
                let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
                let id = TestServer::link_id(&token);
                if expired {
                    server
                        .store()
                        .backdate_link(&id, now - 2 * ONE_HOUR, now - ONE_HOUR)
                        .expect("backdating");
                }
                if revoked {
                    server.store().revoke_link(&id, now).expect("revoking");
                }
                let allow_list = if access {
                    "  - ana@corp.com\n  - bob@corp.com\n"
                } else {
                    "  - bob@corp.com\n"
                };
                server
                    .write_and_rebuild(
                        "index.md",
                        &format!("---\ntitle: Handbook\nallow:\n{allow_list}---\n\n# Handbook\n"),
                    )
                    .await;

                let should_serve = !expired && !revoked && access;
                let status = get(&server, &format!("/s/{token}")).await.status();
                assert_eq!(
                    status == 200,
                    should_serve,
                    "expired={expired} revoked={revoked} access={access} gave {status}"
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 8, "every combination must have been exercised");
}

/// §11, the revocation timing test: revoke from a second connection while the server is
/// running, and the very next request is denied (S21).
#[tokio::test]
async fn a_revocation_takes_effect_on_the_very_next_request() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    let url = format!("/s/{token}");

    assert_eq!(get(&server, &url).await.status(), 200);

    // A genuinely separate connection to the same file, as `mdshelf share revoke` would
    // open from another terminal.
    let database = server
        .state
        .auth
        .as_ref()
        .expect("auth on")
        .settings
        .database_path
        .clone();
    let other = mdshelf::auth::store::Store::open(&database).expect("second connection");
    assert!(
        other
            .revoke_link(&TestServer::link_id(&token), mdshelf::auth::store::now_ms())
            .expect("revoking")
    );
    drop(other);

    assert_eq!(
        get(&server, &url).await.status(),
        404,
        "S21: no cache to invalidate, so a revoke lands on the next request"
    );
}

// ---------------------------------------------------------------------------
// US-11 — audit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn link_reads_are_recorded_under_the_link_pseudonym() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    let id = TestServer::link_id(&token);

    assert_eq!(get(&server, &format!("/s/{token}")).await.status(), 200);
    assert_eq!(
        get(&server, &format!("/s/{token}/img/chart.png"))
            .await
            .status(),
        200
    );
    // A valid token asking for something outside its page.
    assert_eq!(
        get(&server, &format!("/s/{token}/hr/policy"))
            .await
            .status(),
        404
    );

    let entries = server
        .store()
        .access_by_email(&format!("link:{id}"))
        .expect("audit rows");
    assert_eq!(entries.len(), 3, "one row per request: {entries:?}");
    assert!(
        entries.iter().all(|entry| entry.path == "/docs/hr/comp"),
        "every row names the page, never the token-bearing request path: {entries:?}"
    );
    assert_eq!(
        entries.iter().filter(|e| e.outcome == "allow").count(),
        2,
        "the page and its asset"
    );
    assert_eq!(
        entries.iter().filter(|e| e.outcome == "deny").count(),
        1,
        "US-11: a valid token denied a path appends a deny row"
    );

    // The pseudonym is the id `share list` prints, so the two join by eye (S14).
    let links = server
        .store()
        .list_links(mdshelf::auth::store::now_ms(), true, None)
        .expect("links");
    assert!(links.iter().any(|link| link.id == id));
}

#[tokio::test]
async fn an_unknown_token_is_recorded_as_bad_link_without_the_token() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;

    let unknown = "BBBBBBBBBBBBBBBBBBBBBB";
    assert_eq!(get(&server, &format!("/s/{unknown}")).await.status(), 404);
    assert_eq!(get(&server, "/s/malformed").await.status(), 404);

    let rows = server.store().access_by_path("/s").expect("audit rows");
    assert_eq!(rows.len(), 2, "both probes were recorded: {rows:?}");
    assert!(rows.iter().all(|row| row.outcome == "bad-link"));
    for row in &rows {
        for field in [&row.email, &row.path] {
            assert!(!field.contains(unknown), "the token reached the log");
            // Nor any prefix of it long enough to be a fingerprint.
            assert!(
                !field.contains(&unknown[..8]),
                "a token prefix reached the log"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// US-13 — no change without auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn without_auth_the_prefix_is_not_routed() {
    let public = TestServer::start_public(VAULT).await;

    // Whatever the server answers for /s/<token>, it must be the same thing it answers
    // for any other path that names nothing — the share route does not exist.
    let (share_status, share_headers, share_body) =
        fingerprint(get(&public, "/s/AAAAAAAAAAAAAAAAAAAAAA").await).await;
    let (other_status, other_headers, other_body) =
        fingerprint(get(&public, "/nothing-at-all").await).await;

    assert_eq!(share_status, other_status);
    assert_eq!(share_headers, other_headers);
    assert_eq!(share_body, other_body);
    assert!(
        !share_body.contains("This link is not available"),
        "the deny page is a link-feature response and must not appear without auth"
    );

    // And an ordinary page is served exactly as before, with nothing link-shaped on it.
    let page = get(&public, "/docs/hr/comp").await;
    assert_eq!(page.status(), 200);
    let body = page.text().await.expect("body");
    assert!(body.contains(PAGE_BODY));
    for marker in ["/s/", "share-control", "Share", "no-store"] {
        assert!(
            !body.contains(marker),
            "an unauthenticated render must contain no {marker:?}"
        );
    }
}

/// US-5: the sweep runs at startup, not only on the hourly tick.
#[tokio::test]
async fn the_startup_sweep_deletes_long_dead_rows() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let now = mdshelf::auth::store::now_ms();

    let stale = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    let stale_id = TestServer::link_id(&stale);
    server
        .store()
        .backdate_link(&stale_id, now - 200 * 86_400_000, now - 200 * 86_400_000)
        .expect("backdating");
    assert!(
        server
            .store()
            .link_by_id(&stale_id)
            .expect("lookup")
            .is_some(),
        "precondition: the dead row is there to be swept"
    );

    mdshelf::server::spawn_audit_pruner(&server.state);

    for _ in 0..50 {
        if server
            .store()
            .link_by_id(&stale_id)
            .expect("lookup")
            .is_none()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the startup sweep never ran");
}

// ---------------------------------------------------------------------------
// US-12 — live reload for link readers (S27)
// ---------------------------------------------------------------------------

use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;

/// Open the reload socket for a token, or report the HTTP status that refused it.
async fn open_reload_socket(
    server: &TestServer,
    token: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    u16,
> {
    let url = format!(
        "{}/s/{}/__reload",
        server.base_url.replace("http://", "ws://"),
        token
    );
    match tokio_tungstenite::connect_async(url).await {
        Ok((socket, _)) => Ok(socket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(response.status().as_u16())
        }
        Err(error) => panic!("unexpected socket error: {error}"),
    }
}

/// The next message, or `None` if nothing arrives within the grace period.
///
/// The grace period is what makes "pushes nothing" a real assertion rather than a race:
/// the test waits long enough that a push would have arrived if one were coming.
async fn next_message(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<Message> {
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(750), socket.next()).await {
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(Some(Ok(message))) => return Some(message),
            Ok(Some(Err(_))) | Ok(None) => return None,
            Err(_) => return None,
        }
    }
}

#[tokio::test]
async fn the_reading_view_carries_a_reload_socket_under_the_link_prefix() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth_and_live_reload(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);

    let body = get(&server, &format!("/s/{token}"))
        .await
        .text()
        .await
        .expect("body");
    assert!(
        body.contains(&format!("/s/{token}/__reload")),
        "the reading view must point at its own socket"
    );
    assert!(
        !body.contains("/__livereload"),
        "the session-gated socket is useless to a link reader"
    );
}

#[tokio::test]
async fn a_socket_receives_edits_to_its_own_page_and_nothing_else() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth_and_live_reload(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    let mut socket = open_reload_socket(&server, &token).await.expect("upgrade");

    // Editing another page produces a reload event on the broadcast channel, but this
    // socket must not forward it.
    server
        .write_and_rebuild(
            "hr/policy.md",
            "---\ntitle: Policy\n---\n\n# Policy\n\nZZ-OTHER-PAGE-ZZ edited.\n",
        )
        .await;
    let _ = server.state.live_reload_tx.send(());
    assert!(
        next_message(&mut socket).await.is_none(),
        "an edit to another page must push nothing"
    );

    // Editing the shared page does.
    server
        .write_and_rebuild(
            SHARED_PAGE,
            "---\ntitle: Compensation\n---\n\n# Compensation\n\n\
             ![Chart](../img/chart.png)\n\n[Policy](policy.md)\n\n\
             Salary bands for the current year. Revised.\n",
        )
        .await;
    let _ = server.state.live_reload_tx.send(());
    assert_eq!(
        next_message(&mut socket).await,
        Some(Message::Text("reload".into())),
        "an edit to the shared page must push a reload"
    );
}

#[tokio::test]
async fn a_revoked_link_closes_its_socket_instead_of_pushing() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth_and_live_reload(VAULT, &idp).await;
    let token = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    let mut socket = open_reload_socket(&server, &token).await.expect("upgrade");

    server
        .store()
        .revoke_link(&TestServer::link_id(&token), mdshelf::auth::store::now_ms())
        .expect("revoking");
    // Edit the shared page, so a push would definitely be due if the link were alive.
    server
        .write_and_rebuild(
            SHARED_PAGE,
            "---\ntitle: Compensation\n---\n\n# Compensation\n\nRevised again.\n",
        )
        .await;
    let _ = server.state.live_reload_tx.send(());

    match next_message(&mut socket).await {
        Some(Message::Close(Some(frame))) => {
            assert_eq!(u16::from(frame.code), 1008, "US-12 names 1008");
        }
        other => panic!("expected a 1008 close, got {other:?}"),
    }
    // And no reload event was ever delivered.
    assert!(next_message(&mut socket).await.is_none());
}

#[tokio::test]
async fn a_socket_opened_with_an_unknown_token_is_refused() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth_and_live_reload(VAULT, &idp).await;

    for token in [
        "CCCCCCCCCCCCCCCCCCCCCC".to_string(),
        "not-a-token".to_string(),
        server.mint_link(SHARED_PAGE, ISSUER, -ONE_HOUR),
    ] {
        match open_reload_socket(&server, &token).await {
            Ok(mut socket) => {
                let _ = socket.close(None).await;
                panic!("{token} should not have been upgraded");
            }
            Err(status) => assert_eq!(status, 404, "for {token}"),
        }
    }

    // Precondition: a live token *is* upgraded, so the refusals above mean something.
    let live = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    let mut socket = open_reload_socket(&server, &live).await.expect("upgrade");
    let _ = socket.close(None).await;
}

/// US-5: the retention sweep the server runs at startup and hourly covers links and
/// `bad-link` rows, not just the access log.
#[tokio::test]
async fn the_retention_sweep_covers_links_and_bad_link_rows() {
    let idp = MockIdp::start().await;
    let server = start(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth on").clone();
    let now = mdshelf::auth::store::now_ms();
    let long_ago = now - 200 * 86_400_000;

    let live = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    let stale = server.mint_link(SHARED_PAGE, ISSUER, ONE_HOUR);
    let stale_id = TestServer::link_id(&stale);
    server
        .store()
        .backdate_link(&stale_id, long_ago, long_ago)
        .expect("backdating");

    // One recent bad-link probe and one from long enough ago to be past its window.
    assert_eq!(
        get(&server, "/s/DDDDDDDDDDDDDDDDDDDDDD").await.status(),
        404
    );
    server
        .store()
        .log_access(
            "link:unknown",
            "/s",
            now - 30 * 86_400_000,
            mdshelf::auth::store::Outcome::BadLink,
        )
        .expect("logging");
    assert_eq!(
        server.store().access_by_path("/s").expect("rows").len(),
        2,
        "precondition: both probes are recorded"
    );

    tokio::task::spawn_blocking(move || runtime.prune_audit())
        .await
        .expect("sweep");

    assert!(
        server
            .store()
            .link_by_id(&stale_id)
            .expect("lookup")
            .is_none(),
        "a link dead longer than revoked_retention is deleted"
    );
    assert!(
        server
            .store()
            .link_by_token_hash(&mdshelf::links::token_hash(&live))
            .expect("lookup")
            .is_some(),
        "a live link survives"
    );
    let rows = server.store().access_by_path("/s").expect("rows");
    assert_eq!(rows.len(), 1, "only the recent probe survives: {rows:?}");
}
