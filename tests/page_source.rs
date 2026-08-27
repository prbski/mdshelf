//! The raw-Markdown download route, `GET /__mdshelf/md/{*path}`.
//!
//! The route exists only because iOS Safari mishandles Blob downloads — every other
//! browser serves the copy embedded in the page. That makes it a small surface with a
//! large blast radius: it is a *second* answer to "which page is this URL, and may you
//! read it?", and the failure mode of drift is disclosure.
//!
//! So the assertions here are mostly about sameness. Denied, missing, draft and
//! "no site matches" must leave the server as the same bytes, and an anonymous visitor
//! must get the same interstitial a page request returns. The response-shape checks
//! (MIME, `Content-Disposition`, exact body) come second.

use mdshelf::auth::SESSION_COOKIE;
use mdshelf::test_support::{MockIdp, TestServer, TokenSpec, client};

const SECRET_BODY: &str = "ZZTOPSECRETZZ";

const VAULT: &[(&str, &str)] = &[
    (
        "index.md",
        "---\ntitle: Handbook\nallow:\n  - team@corp.com\n---\n\n# Handbook\n",
    ),
    (
        "guides/setup.md",
        "---\ntitle: Setup Guide\n---\n\n# Setup Guide\n\nRun it.\n",
    ),
    // No leading H1, so `source_text` has to prepend the title.
    (
        "guides/no-heading.md",
        "---\ntitle: Loose Notes\n---\n\nJust a paragraph.\n",
    ),
    // A literal script closer and comment opener: the two sequences the escaper exists
    // for. The route serves the raw bytes, so they must arrive unescaped.
    (
        "guides/adversarial.md",
        "---\ntitle: Adversarial\n---\n\n# Adversarial\n\nA closer </script> and \
         an opener <!-- here --> and a backslash \\ and <\\/ verbatim.\n",
    ),
    (
        "guides/draft.md",
        "---\ntitle: Draft Page\ndraft: true\n---\n\n# Draft Page\n\nUnpublished.\n",
    ),
    ("guides/настройка.md", "# Настройка\n\nТекст.\n"),
    // A folder with no index.md, so `/docs/folder` renders a generated listing that has
    // no `.md` file behind it.
    ("folder/inner.md", "# Inner\n\nx\n"),
    (
        "hr/index.md",
        "---\ntitle: People Ops\nallow:\n  - hr@corp.com\ndeny:\n  - team@corp.com\n---\n\n# People Ops\n",
    ),
    (
        "hr/salaries.md",
        "---\ntitle: Executive Compensation Q3\n---\n\n# Executive Compensation Q3\n\nZZTOPSECRETZZ\n",
    ),
];

fn query_params(url: &str) -> std::collections::HashMap<String, String> {
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

/// Status, every header, and the body — the whole observable response.
async fn fingerprint(response: reqwest::Response) -> (u16, Vec<(String, String)>, String) {
    let status = response.status().as_u16();
    let mut headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .filter(|(name, _)| {
            // Varies per response by construction, not by authorization.
            name.as_str() != "date" && name.as_str() != "content-length"
        })
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    headers.sort();
    let body = response.text().await.expect("body");
    (status, headers, body)
}

fn disposition(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .expect("a Content-Disposition header")
        .to_str()
        .expect("printable Content-Disposition")
        .to_string()
}

fn content_type(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .expect("a Content-Type header")
        .to_str()
        .expect("printable Content-Type")
        .to_string()
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unrestricted_server_serves_the_source_with_its_own_filename() {
    let server = TestServer::start_public(VAULT).await;
    let response = client()
        .get(server.url("/__mdshelf/md/docs/guides/setup"))
        .send()
        .await
        .expect("request");

    assert_eq!(response.status(), 200);
    assert_eq!(content_type(&response), "text/markdown; charset=utf-8");
    assert_eq!(
        disposition(&response),
        "attachment; filename=\"setup.md\"; filename*=UTF-8''setup.md",
        "the on-disk basename, so the file round-trips back into the vault"
    );
    assert_eq!(
        response.text().await.expect("body"),
        "# Setup Guide\n\nRun it.\n",
        "frontmatter excluded, body verbatim, one trailing newline"
    );
}

#[tokio::test]
async fn a_page_without_a_leading_h1_gains_its_title() {
    let server = TestServer::start_public(VAULT).await;
    let response = client()
        .get(server.url("/__mdshelf/md/docs/guides/no-heading"))
        .send()
        .await
        .expect("request");

    assert_eq!(response.status(), 200);
    assert_eq!(
        response.text().await.expect("body"),
        "# Loose Notes\n\nJust a paragraph.\n"
    );
}

/// The route serves raw Markdown, so the sequences the *embed* has to escape must
/// arrive here completely untouched.
#[tokio::test]
async fn the_route_serves_raw_bytes_with_nothing_escaped() {
    let server = TestServer::start_public(VAULT).await;
    let body = client()
        .get(server.url("/__mdshelf/md/docs/guides/adversarial"))
        .send()
        .await
        .expect("request")
        .text()
        .await
        .expect("body");

    assert!(body.contains("</script>"), "got: {body:?}");
    assert!(body.contains("<!-- here -->"), "got: {body:?}");
    assert!(body.contains("a backslash \\ and"), "got: {body:?}");
    assert!(body.contains("<\\/ verbatim"), "got: {body:?}");
    assert!(
        !body.contains("<\\/script>"),
        "the route must not apply the script-block escaping: {body:?}"
    );
}

#[tokio::test]
async fn a_non_ascii_filename_is_named_twice() {
    let server = TestServer::start_public(VAULT).await;
    let response = client()
        .get(server.url("/__mdshelf/md/docs/guides/настройка"))
        .send()
        .await
        .expect("request");

    assert_eq!(response.status(), 200);
    let value = disposition(&response);
    assert!(
        value
            .contains("filename*=UTF-8''%D0%BD%D0%B0%D1%81%D1%82%D1%80%D0%BE%D0%B9%D0%BA%D0%B0.md"),
        "the real name has to survive for a browser that reads filename*: {value}"
    );
    assert!(
        value.contains("filename=\""),
        "and an ASCII reduction for one that does not: {value}"
    );
    assert!(
        value.is_ascii(),
        "a header value must be ASCII throughout: {value}"
    );
}

/// `get()` also answers `HEAD`, which the browser's reachability probe depends on
/// before it hands the download to the server instead of a Blob.
#[tokio::test]
async fn head_mirrors_get() {
    let server = TestServer::start_public(VAULT).await;
    let head = client()
        .head(server.url("/__mdshelf/md/docs/guides/setup"))
        .send()
        .await
        .expect("request");

    assert_eq!(head.status(), 200);
    assert_eq!(content_type(&head), "text/markdown; charset=utf-8");

    let missing = client()
        .head(server.url("/__mdshelf/md/docs/nope"))
        .send()
        .await
        .expect("request");
    assert_eq!(
        missing.status(),
        404,
        "the probe has to be able to tell a missing route from a live one"
    );
}

// ---------------------------------------------------------------------------
// Authorization — the reason this suite exists
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_invitee_gets_the_source_and_everyone_else_gets_the_deny_page() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    let hr = sign_in(&server, &idp, "hr@corp.com").await;
    let allowed = get_as(&server, &hr, "/__mdshelf/md/docs/hr/salaries").await;
    assert_eq!(allowed.status(), 200);
    assert!(
        allowed.text().await.expect("body").contains(SECRET_BODY),
        "the invitee may read it"
    );

    let team = sign_in(&server, &idp, "team@corp.com").await;
    let denied = get_as(&server, &team, "/__mdshelf/md/docs/hr/salaries").await;
    assert_eq!(denied.status(), 404);
    assert!(
        !denied.text().await.expect("body").contains(SECRET_BODY),
        "and nobody else does"
    );
}

/// D23, on this route. A path the viewer may not read, a path that does not exist, a
/// draft, and a path under no configured site must be indistinguishable — not merely
/// equal in status, but byte-identical, header for header.
#[tokio::test]
async fn denied_missing_draft_and_unmatched_are_byte_identical() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let team = sign_in(&server, &idp, "team@corp.com").await;

    let baseline =
        fingerprint(get_as(&server, &team, "/__mdshelf/md/docs/hr/salaries").await).await;

    for path in [
        // Does not exist.
        "/__mdshelf/md/docs/guides/nope",
        // `draft: true` — unpublished has to look like unreadable.
        "/__mdshelf/md/docs/guides/draft",
        // A generated folder listing: rendered as a page, but no `.md` behind it.
        "/__mdshelf/md/docs/folder",
        // Restricted folder index.
        "/__mdshelf/md/docs/hr",
        // No configured site owns this prefix.
        "/__mdshelf/md/nosuchsite/page",
        // The route with no page path at all.
        "/__mdshelf/md/",
    ] {
        let other = fingerprint(get_as(&server, &team, path).await).await;
        assert_eq!(
            other, baseline,
            "{path} must be byte-identical to a restricted page, or the route can be \
             used to enumerate the vault"
        );
    }
}

#[tokio::test]
async fn an_anonymous_visitor_gets_the_same_interstitial_a_page_gives() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    let page = fingerprint(
        client()
            .get(server.url("/docs/guides/setup"))
            .send()
            .await
            .expect("request"),
    )
    .await;
    let route = fingerprint(
        client()
            .get(server.url("/__mdshelf/md/docs/guides/setup"))
            .send()
            .await
            .expect("request"),
    )
    .await;

    assert_eq!(page.0, 401, "SEC-9: a page request interstitials");
    assert_eq!(
        route, page,
        "and so must this route, or it tells an anonymous visitor what exists"
    );
}

/// Regression guard shared with the leak suite: a case-variant path must resolve
/// through the same ACL as the canonical one.
#[tokio::test]
async fn a_case_variant_path_does_not_bypass_a_folder_rule() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let team = sign_in(&server, &idp, "team@corp.com").await;

    for path in [
        "/__mdshelf/md/docs/hr/salaries",
        "/__mdshelf/md/docs/HR/salaries",
        "/__mdshelf/md/docs/hr/salaries.md",
        "/__mdshelf/md/docs/hr/SALARIES.MD",
    ] {
        let response = get_as(&server, &team, path).await;
        assert_eq!(response.status(), 404, "{path}");
        assert!(
            !response.text().await.expect("body").contains(SECRET_BODY),
            "{path} leaked restricted bytes"
        );
    }
}

/// NFR-2: with auth off, the route is simply open, and a missing path is missing —
/// there is nothing to enumerate on a server that publishes everything.
#[tokio::test]
async fn without_auth_a_missing_path_is_a_plain_404() {
    let server = TestServer::start_public(VAULT).await;
    for path in [
        "/__mdshelf/md/docs/guides/nope",
        "/__mdshelf/md/docs/guides/draft",
        "/__mdshelf/md/docs/folder",
        "/__mdshelf/md/nosuchsite/page",
    ] {
        let response = client()
            .get(server.url(path))
            .send()
            .await
            .expect("request");
        assert_eq!(response.status(), 404, "{path}");
    }

    // And the restricted-by-nobody page is served, because nothing is restricted.
    let response = client()
        .get(server.url("/__mdshelf/md/docs/hr/salaries"))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), 200);
    assert!(response.text().await.expect("body").contains(SECRET_BODY));
}

// ---------------------------------------------------------------------------
// The embedded block and the route agree
// ---------------------------------------------------------------------------

/// D21: the route serves `Page.body` from memory, and the page embeds the same value,
/// so the two can never disagree. Verified by decoding the embed and comparing.
#[tokio::test]
async fn the_embedded_block_and_the_route_serve_the_same_bytes() {
    let server = TestServer::start_public(VAULT).await;

    for path in ["guides/setup", "guides/no-heading", "guides/adversarial"] {
        let html = client()
            .get(server.url(&format!("/docs/{path}")))
            .send()
            .await
            .expect("request")
            .text()
            .await
            .expect("body");
        let embedded = decode_embedded_source(&html)
            .unwrap_or_else(|| panic!("{path} carried no source block"));

        let served = client()
            .get(server.url(&format!("/__mdshelf/md/docs/{path}")))
            .send()
            .await
            .expect("request")
            .text()
            .await
            .expect("body");

        assert_eq!(embedded, served, "{path}");
    }
}

/// A generated folder listing has no file behind it, so it carries no source block and
/// its page-actions items are disabled.
#[tokio::test]
async fn a_generated_listing_carries_no_source_block() {
    let server = TestServer::start_public(VAULT).await;

    for path in ["/docs/folder", "/"] {
        let html = client()
            .get(server.url(path))
            .send()
            .await
            .expect("request")
            .text()
            .await
            .expect("body");
        assert!(
            !html.contains("type=\"text/markdown\""),
            "{path} must not embed a source block"
        );
    }

    let listing = client()
        .get(server.url("/docs/folder"))
        .send()
        .await
        .expect("request")
        .text()
        .await
        .expect("body");
    assert_eq!(
        listing.matches("This page has no Markdown file").count(),
        2,
        "both items are disabled, with the reason"
    );
}

/// The inverse of the server-side escaper, as the theme's JavaScript implements it: on
/// a backslash, take the next character literally.
fn decode_embedded_source(html: &str) -> Option<String> {
    let start = html.find("<script type=\"text/markdown\"")?;
    let open_end = html[start..].find('>')? + start + 1;
    let close = html[open_end..].find("</script>")? + open_end;
    let escaped = &html[open_end..close];

    let mut out = String::with_capacity(escaped.len());
    let mut chars = escaped.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(ch);
        }
    }
    Some(out)
}
