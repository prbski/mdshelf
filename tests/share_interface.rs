//! Phase 3 — the interface (US-14 … US-17).
//!
//! The control, the mint and revoke endpoints, and the `Shared by you` page, driven
//! over HTTP against a real server exactly as the browser drives them.

use std::collections::HashMap;

use mdshelf::auth::SESSION_COOKIE;
use mdshelf::test_support::{MockIdp, TestServer, TestSite, TokenSpec, client};

const ANA: &str = "ana@corp.com";
const BOB: &str = "bob@corp.com";

const VAULT: &[(&str, &str)] = &[
    (
        "index.md",
        "---\ntitle: Handbook\nallow:\n  - ana@corp.com\n  - bob@corp.com\n---\n\n# Handbook\n",
    ),
    (
        "hr/comp.md",
        "---\ntitle: Compensation\n---\n\n# Compensation\n\nSalary bands.\n",
    ),
    (
        "secret/index.md",
        "---\ntitle: Secret\nallow:\n  - bob@corp.com\ndeny:\n  - ana@corp.com\n---\n\n# Secret\n",
    ),
    (
        "secret/plan.md",
        "---\ntitle: Plan\n---\n\n# Plan\n\nZZ-SECRET-ZZ\n",
    ),
];

const NOTES: &[(&str, &str)] = &[(
    "index.md",
    "---\ntitle: Notes\nallow:\n  - ana@corp.com\n---\n\n# Notes\n",
)];

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

async fn post_as(
    server: &TestServer,
    cookie: Option<&str>,
    path: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    let mut request = client().post(server.url(path)).json(&body);
    if let Some(cookie) = cookie {
        request = request.header(
            reqwest::header::COOKIE,
            format!("{SESSION_COOKIE}={cookie}"),
        );
    }
    request.send().await.expect("request")
}

async fn body_of(response: reqwest::Response) -> String {
    response.text().await.expect("body")
}

// ---------------------------------------------------------------------------
// US-14 — the control appears where it should
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_signed_in_viewer_sees_the_control_on_every_page_they_can_read() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, ANA).await;

    for path in ["/docs", "/docs/hr/comp"] {
        let body = body_of(get_as(&server, &cookie, path).await).await;
        assert!(
            body.contains("mdshelf-share-button"),
            "{path} should carry the Share control"
        );
    }
    let page = body_of(get_as(&server, &cookie, "/docs/hr/comp").await).await;
    assert!(page.contains("id=\"mdshelf-share-panel\""));
    assert!(page.contains("data-page=\"/docs/hr/comp\""));

    // A page ana may not read never renders at all, so it cannot carry a control.
    let denied = get_as(&server, &cookie, "/docs/secret/plan").await;
    assert_eq!(denied.status(), 404);
    assert!(!body_of(denied).await.contains("mdshelf-share"));
}

#[tokio::test]
async fn an_anonymous_visitor_and_a_link_reader_never_receive_the_control() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    let anonymous = client()
        .get(server.url("/docs/hr/comp"))
        .send()
        .await
        .expect("request");
    assert_eq!(anonymous.status(), 401, "the interstitial");
    assert!(!body_of(anonymous).await.contains("mdshelf-share"));

    let token = server.mint_link("hr/comp.md", ANA, 3_600_000);
    let reading_view = client()
        .get(server.url(&format!("/s/{token}")))
        .send()
        .await
        .expect("request");
    assert_eq!(reading_view.status(), 200);
    let body = body_of(reading_view).await;
    assert!(
        !body.contains("mdshelf-share") && !body.contains("__share"),
        "a recipient must not be offered a control they cannot use"
    );
}

#[tokio::test]
async fn with_auth_off_no_page_contains_the_control() {
    let server = TestServer::start_public(VAULT).await;
    for path in ["/", "/docs", "/docs/hr/comp", "/docs/secret/plan"] {
        let body = body_of(
            client()
                .get(server.url(path))
                .send()
                .await
                .expect("request"),
        )
        .await;
        assert!(
            !body.contains("mdshelf-share"),
            "{path} carried the control on an unauthenticated server"
        );
    }

    // NFR-1 is about bytes, not just about the absence of a string. The placeholder
    // sits at the end of its line precisely so an empty control leaves the markup
    // exactly as it was before this feature existed.
    let page = body_of(
        client()
            .get(server.url("/docs/hr/comp"))
            .send()
            .await
            .expect("request"),
    )
    .await;
    assert!(
        page.contains("<div class=\"doc-header-actions\">\n    <button"),
        "an empty control must leave no whitespace behind: {page}"
    );
}

/// US-14/R5: a theme that never mentions `share_control` renders without it, and
/// without error.
#[test]
fn a_theme_without_the_partial_renders_without_it_and_without_error() {
    use mdshelf::render::Renderer;
    use mdshelf::render::templates::{
        ConfigSummary, Crumb, PageContext, PageTemplateContext, SiteContext,
    };
    use mdshelf::theme::ThemeStack;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("layouts")).unwrap();
    std::fs::write(
        dir.path().join("layouts/doc.html"),
        "<html><body><h1>{{ page.title }}</h1>{{ page.html | safe }}</body></html>",
    )
    .unwrap();

    let mut config = mdshelf::config::Config::for_test(
        dir.path().to_path_buf(),
        vec![mdshelf::config::SiteConfig::for_test(dir.path())],
    );
    config.theme.directory = Some(dir.path().to_path_buf());

    let theme = ThemeStack::from_config(&config).expect("theme");
    let renderer = Renderer::new(&theme).expect("renderer");

    let html = renderer
        .render_page(&PageTemplateContext {
            site: SiteContext {
                title: "Docs".into(),
                mount: "/docs".into(),
                root_url: "/docs".into(),
                color: "#10b981".into(),
            },
            page: PageContext {
                title: "Compensation".into(),
                description: None,
                url: "/docs/hr/comp".into(),
                url_path: "hr/comp".into(),
                layout: "doc".into(),
                draft: false,
                headings: vec![],
                frontmatter: serde_json::json!({}),
                html: "<p>Body.</p>".into(),
                source_escaped: Some("# Compensation\n".into()),
                md_url: Some("/__mdshelf/md/docs/hr/comp".into()),
                source_filename: Some("comp.md".into()),
            },
            nav_flat: std::sync::Arc::new(vec![]),
            breadcrumbs: vec![Crumb {
                title: "Docs".into(),
                url: "/docs".into(),
            }],
            prev: None,
            next: None,
            site_index: None,
            all_sites: vec![],
            config: ConfigSummary {
                version: "test".into(),
                theme_name: None,
            },
            live_reload: false,
            // The control is offered; this theme simply never places it.
            share_control: "<div id=\"mdshelf-share\">CONTROL</div>".into(),
        })
        .expect("a theme without the partial must still render");

    assert!(html.contains("Compensation"));
    assert!(!html.contains("CONTROL"), "the theme did not ask for it");
}

// ---------------------------------------------------------------------------
// US-15 — creating a link from the popover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_popover_offers_the_preset_chips_the_custom_date_and_the_configured_default() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, ANA).await;
    let body = body_of(get_as(&server, &cookie, "/docs/hr/comp").await).await;

    for value in [
        "5m", "15m", "30m", "1h", "4h", "8h", "1d", "2d", "3d", "1w", "2w", "30d",
    ] {
        assert!(
            body.contains(&format!("value=\"{value}\"")),
            "missing the {value} chip"
        );
    }
    // The shipped default is 1d.
    assert!(body.contains("value=\"1d\" checked"), "got: {body}");
    // Custom expiry is its own, bigger button; the date field is what it reveals.
    assert!(
        body.contains("id=\"mdshelf-share-custom\""),
        "a custom-date button"
    );
    assert!(body.contains("id=\"mdshelf-share-copy\""), "a copy control");
    assert!(
        body.contains("cannot be shown again"),
        "the URL is shown once and that has to be said"
    );
    // The cap is visible to the browser as well as enforced by the server.
    assert!(body.contains("type=\"date\""), "got: {body}");
    assert!(body.contains("max=\""));
    assert!(body.contains("min=\""));
}

#[tokio::test]
async fn minting_returns_a_url_and_stores_the_sessions_own_address_as_issuer() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, ANA).await;

    let response = post_as(
        &server,
        Some(&cookie),
        "/__share",
        // The body carries an issuer field, which the server must ignore entirely.
        serde_json::json!({ "url": "/docs/hr/comp", "for": "1h", "issued_by": BOB }),
    )
    .await;
    assert_eq!(response.status(), 200);
    let payload: serde_json::Value = response.json().await.expect("json");
    let url = payload["url"].as_str().expect("a url").to_string();
    let id = payload["id"].as_str().expect("an id").to_string();

    assert!(url.contains("/s/"), "got: {url}");
    let token = url.rsplit('/').next().expect("a token");
    let record = server
        .store()
        .link_by_token_hash(&mdshelf::links::token_hash(token))
        .expect("lookup")
        .expect("the row exists");
    assert_eq!(
        record.issued_by, ANA,
        "the issuer is the session's verified address, never the request body's"
    );
    assert_eq!(record.id, id);
    assert_eq!(record.path, "hr/comp.md");

    // And it really works, straight away.
    let read = client()
        .get(server.url(&format!("/s/{token}")))
        .send()
        .await
        .expect("request");
    assert_eq!(read.status(), 200);
    assert!(body_of(read).await.contains("Salary bands"));
}

#[tokio::test]
async fn a_date_beyond_the_cap_is_refused_by_the_server_too() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, ANA).await;

    let response = post_as(
        &server,
        Some(&cookie),
        "/__share",
        serde_json::json!({ "url": "/docs/hr/comp", "until": "2099-01-01" }),
    )
    .await;
    assert_eq!(
        response.status(),
        400,
        "the browser's `max` attribute is a convenience, not the enforcement"
    );
    assert!(
        body_of(response).await.contains("max_lifetime"),
        "the refusal should name the cap"
    );

    // Precondition: a date inside the cap is accepted by the same endpoint.
    let ok = post_as(
        &server,
        Some(&cookie),
        "/__share",
        serde_json::json!({ "url": "/docs/hr/comp", "for": "1d" }),
    )
    .await;
    assert_eq!(ok.status(), 200);
}

#[tokio::test]
async fn a_post_without_a_session_is_refused() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    for path in ["/__share", "/__share/revoke"] {
        let response = post_as(
            &server,
            None,
            path,
            serde_json::json!({ "url": "/docs/hr/comp", "id": "aaaaaa" }),
        )
        .await;
        assert_eq!(response.status(), 401, "{path} accepted an anonymous POST");
    }
    // The precondition: the same endpoint accepts the same body with a session.
    let cookie = sign_in(&server, &idp, ANA).await;
    let allowed = post_as(
        &server,
        Some(&cookie),
        "/__share",
        serde_json::json!({ "url": "/docs/hr/comp", "for": "1h" }),
    )
    .await;
    assert_eq!(allowed.status(), 200);
    let id = allowed.json::<serde_json::Value>().await.expect("json")["id"]
        .as_str()
        .expect("id")
        .to_string();
    server
        .store()
        .revoke_link(&id, mdshelf::auth::store::now_ms())
        .expect("tidy up so the count below is about the anonymous POSTs");
    let minted = server
        .store()
        .list_links(mdshelf::auth::store::now_ms(), true, None)
        .expect("links");
    assert_eq!(
        minted.len(),
        1,
        "only the signed-in POST minted anything: {minted:?}"
    );
}

/// US-15: a page the session cannot read and a page that does not exist are the same
/// answer, byte for byte.
#[tokio::test]
async fn an_unreadable_page_and_a_missing_page_are_the_same_refusal() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, ANA).await;

    let unreadable = post_as(
        &server,
        Some(&cookie),
        "/__share",
        serde_json::json!({ "url": "/docs/secret/plan", "for": "1h" }),
    )
    .await;
    let missing = post_as(
        &server,
        Some(&cookie),
        "/__share",
        serde_json::json!({ "url": "/docs/there-is-no-such-page", "for": "1h" }),
    )
    .await;

    assert_eq!(unreadable.status(), missing.status());
    let unreadable_status = unreadable.status();
    let unreadable_body = body_of(unreadable).await;
    assert_eq!(unreadable_body, body_of(missing).await);
    assert_eq!(unreadable_status, 404);
    assert!(!unreadable_body.contains("ZZ-SECRET-ZZ"));
    assert!(!unreadable_body.contains("secret"));

    // Precondition: bob, who may read it, gets a link for the very same page.
    let bob = sign_in(&server, &idp, BOB).await;
    let allowed = post_as(
        &server,
        Some(&bob),
        "/__share",
        serde_json::json!({ "url": "/docs/secret/plan", "for": "1h" }),
    )
    .await;
    assert_eq!(allowed.status(), 200, "the refusal above was about access");
}

// ---------------------------------------------------------------------------
// US-16 — per-page listing and revoke
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_popover_lists_this_viewers_links_for_this_page_and_no_one_elses() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let ana = sign_in(&server, &idp, ANA).await;

    let mine = post_as(
        &server,
        Some(&ana),
        "/__share",
        serde_json::json!({ "url": "/docs/hr/comp", "for": "1h" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .expect("json");
    let mine_id = mine["id"].as_str().expect("id").to_string();

    // A link the same viewer made for a *different* page, and one somebody else made
    // for this page.
    let elsewhere = post_as(
        &server,
        Some(&ana),
        "/__share",
        serde_json::json!({ "url": "/docs", "for": "1h" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .expect("json");
    let elsewhere_id = elsewhere["id"].as_str().expect("id").to_string();

    let bob = sign_in(&server, &idp, BOB).await;
    let theirs = post_as(
        &server,
        Some(&bob),
        "/__share",
        serde_json::json!({ "url": "/docs/hr/comp", "for": "1h" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .expect("json");
    let theirs_id = theirs["id"].as_str().expect("id").to_string();

    let body = body_of(get_as(&server, &ana, "/docs/hr/comp").await).await;
    assert!(body.contains(&mine_id), "my link for this page is listed");
    assert!(
        !body.contains(&theirs_id),
        "another person's link for this page must never be listed"
    );
    assert!(
        !body.contains(&elsewhere_id),
        "my link for another page belongs in Shared by you, not here"
    );
    assert!(body.contains(&format!("data-revoke=\"{mine_id}\"")));
}

#[tokio::test]
async fn revoking_from_the_popover_kills_the_link_on_the_next_request() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let ana = sign_in(&server, &idp, ANA).await;

    let created = post_as(
        &server,
        Some(&ana),
        "/__share",
        serde_json::json!({ "url": "/docs/hr/comp", "for": "1h" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .expect("json");
    let id = created["id"].as_str().expect("id").to_string();
    let url = created["url"].as_str().expect("url").to_string();
    let token = url.rsplit('/').next().expect("token").to_string();

    assert_eq!(
        client()
            .get(server.url(&format!("/s/{token}")))
            .send()
            .await
            .expect("request")
            .status(),
        200
    );

    let revoke = post_as(
        &server,
        Some(&ana),
        "/__share/revoke",
        serde_json::json!({ "id": id }),
    )
    .await;
    assert_eq!(revoke.status(), 200);

    assert_eq!(
        client()
            .get(server.url(&format!("/s/{token}")))
            .send()
            .await
            .expect("request")
            .status(),
        404,
        "a revoke lands on the very next request"
    );

    // And it is gone from the live listing the popover renders.
    let body = body_of(get_as(&server, &ana, "/docs/hr/comp").await).await;
    assert!(
        !body.contains(&format!("data-revoke=\"{id}\"")),
        "got: {body}"
    );
    assert!(body.contains("No live links for this page."));
}

#[tokio::test]
async fn revoking_someone_elses_link_is_refused_exactly_like_an_unknown_id() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let bob = sign_in(&server, &idp, BOB).await;
    let theirs = post_as(
        &server,
        Some(&bob),
        "/__share",
        serde_json::json!({ "url": "/docs/hr/comp", "for": "1h" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .expect("json");
    let theirs_id = theirs["id"].as_str().expect("id").to_string();

    let ana = sign_in(&server, &idp, ANA).await;
    let not_mine = post_as(
        &server,
        Some(&ana),
        "/__share/revoke",
        serde_json::json!({ "id": theirs_id }),
    )
    .await;
    let unknown = post_as(
        &server,
        Some(&ana),
        "/__share/revoke",
        serde_json::json!({ "id": "ffffff" }),
    )
    .await;

    assert_eq!(not_mine.status(), unknown.status());
    assert_eq!(not_mine.status(), 404);
    assert_eq!(body_of(not_mine).await, body_of(unknown).await);
    assert!(
        server
            .store()
            .link_by_id(&theirs_id)
            .expect("lookup")
            .expect("still there")
            .revoked_at
            .is_none(),
        "somebody else's link must be untouched"
    );
}

// ---------------------------------------------------------------------------
// US-17 — Shared by you
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shared_by_you_lists_every_live_link_this_viewer_issued_across_sites() {
    let idp = MockIdp::start().await;
    let sites = [
        TestSite {
            mount: "/docs",
            title: "Docs",
            files: VAULT,
        },
        TestSite {
            mount: "/notes",
            title: "Notes",
            files: NOTES,
        },
    ];
    let server = TestServer::start_with_auth_sites(&sites, &idp).await;
    let ana = sign_in(&server, &idp, ANA).await;

    let mut mine = Vec::new();
    for url in ["/docs/hr/comp", "/notes"] {
        let created = post_as(
            &server,
            Some(&ana),
            "/__share",
            serde_json::json!({ "url": url, "for": "1h" }),
        )
        .await;
        assert_eq!(created.status(), 200, "creating a link for {url}");
        mine.push(
            created
                .json::<serde_json::Value>()
                .await
                .expect("json")
                .get("id")
                .and_then(|id| id.as_str())
                .expect("id")
                .to_string(),
        );
    }

    let bob = sign_in(&server, &idp, BOB).await;
    let theirs = post_as(
        &server,
        Some(&bob),
        "/__share",
        serde_json::json!({ "url": "/docs/hr/comp", "for": "1h" }),
    )
    .await
    .json::<serde_json::Value>()
    .await
    .expect("json");
    let theirs_id = theirs["id"].as_str().expect("id").to_string();

    let page = body_of(get_as(&server, &ana, "/__shares").await).await;
    for id in &mine {
        assert!(page.contains(id), "{id} is missing from Shared by you");
    }
    assert!(
        !page.contains(&theirs_id),
        "another person's link must never appear"
    );
    assert!(page.contains("/docs/hr/comp"), "each row names its page");
    assert!(page.contains("/notes"), "across all sites");
    // Rounded down, so an hour-long link reads "59 minutes" rather than promising time
    // it does not have.
    assert_eq!(
        page.matches("<td>in ").count(),
        2,
        "each row shows how long is left: {page}"
    );
    assert!(
        page.contains("data-revoke="),
        "each row has a revoke control"
    );

    // A revoked link drops out of the listing.
    let revoke = post_as(
        &server,
        Some(&ana),
        "/__share/revoke",
        serde_json::json!({ "id": mine[0] }),
    )
    .await;
    assert_eq!(revoke.status(), 200);
    let page = body_of(get_as(&server, &ana, "/__shares").await).await;
    assert!(!page.contains(&mine[0]));
    assert!(page.contains(&mine[1]));
}

#[tokio::test]
async fn shared_by_you_offers_sign_in_to_a_stranger_and_the_deny_page_to_a_link_reader() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    let anonymous = client()
        .get(server.url("/__shares"))
        .send()
        .await
        .expect("request");
    assert_eq!(anonymous.status(), 401);
    let body = body_of(anonymous).await;
    assert!(body.contains("Sign in with Google"), "the interstitial");
    assert!(!body.contains("Shared by you"));

    // Everything a link reader can reach lives under the prefix, so that is where they
    // would ask for it — and there they get the deny page, like any other path that is
    // not their one page.
    let token = server.mint_link("hr/comp.md", ANA, 3_600_000);
    let response = client()
        .get(server.url(&format!("/s/{token}/__shares")))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), 404);
    let body = body_of(response).await;
    assert!(body.contains("This link is not available"));
    assert!(!body.contains("Shared by you"));
}

/// A signed-in viewer with no links is told so, rather than shown an empty page.
#[tokio::test]
async fn shared_by_you_says_so_when_there_is_nothing_to_show() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let ana = sign_in(&server, &idp, ANA).await;
    let page = body_of(get_as(&server, &ana, "/__shares").await).await;
    assert!(page.contains("You have no live links."), "got: {page}");
}
