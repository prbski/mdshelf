//! Site search.
//!
//! qmd ranks the corpus for nobody in particular, so the property that matters is what
//! mdshelf lets through: a reader sees only pages they could open, a ranking of pages
//! they cannot open is indistinguishable from no ranking at all, and no access rule
//! ever reaches the index or a snippet. The mock index ranks whatever the test asks,
//! which is how a restricted page gets put first.

use std::collections::HashMap;

use mdshelf::auth::SESSION_COOKIE;
use mdshelf::test_support::{MockIdp, MockQmd, TestServer, TestSite, TokenSpec, client};
use serde_json::Value;

const SECRET_TITLE: &str = "Executive Compensation Q3";
const SECRET_BODY: &str = "ZZTOPSECRETZZ";

const VAULT: &[(&str, &str)] = &[
    (
        "index.md",
        "---\ntitle: Handbook\nallow:\n  - team@corp.com\n  - hr@corp.com\n---\n\n# Handbook\n\nThe company handbook.\n",
    ),
    (
        "onboarding.md",
        "---\ntitle: Onboarding\n---\n\n# Onboarding\n\nWelcome aboard. Laptops arrive on day one.\n",
    ),
    (
        "hr/index.md",
        "---\ntitle: People Ops\ndeny:\n  - team@corp.com\n---\n\n# People Ops\n",
    ),
    (
        "hr/salaries.md",
        "---\ntitle: Executive Compensation Q3\n---\n\n# Executive Compensation Q3\n\nZZTOPSECRETZZ laptops for executives.\n",
    ),
    (
        "plans.md",
        "---\ntitle: Plans\ndraft: true\n---\n\n# Plans\n\nUnpublished laptop plans.\n",
    ),
    (
        "Odd Dir/My Notes.md",
        "---\ntitle: My Notes\n---\n\nNotes about laptop stickers.\n",
    ),
];

/// Every page of the vault, best first, as qmd would name them.
const EVERYTHING: &[&str] = &[
    "qmd://docs/hr/salaries.md",
    "qmd://docs/plans.md",
    "qmd://docs/hr/index.md",
    "qmd://docs/onboarding.md",
    "qmd://docs/gone.md",
    "qmd://docs/Odd%20Dir/My%20Notes.md",
    "qmd://docs/index.md",
];

fn sites() -> [TestSite<'static>; 1] {
    [TestSite {
        mount: "/docs",
        title: "Docs",
        files: VAULT,
    }]
}

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

/// Status and body of a request, as the reader would receive them.
async fn fetch(server: &TestServer, cookie: Option<&str>, path: &str) -> (u16, String) {
    let mut request = client().get(server.url(path));
    if let Some(cookie) = cookie {
        request = request.header(
            reqwest::header::COOKIE,
            format!("{SESSION_COOKIE}={cookie}"),
        );
    }
    let response = request.send().await.expect("request");
    let status = response.status().as_u16();
    (status, response.text().await.expect("body"))
}

async fn results(server: &TestServer, cookie: Option<&str>, query: &str) -> Value {
    let path = format!(
        "/__search.json?q={}",
        url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>()
    );
    let (status, body) = fetch(server, cookie, &path).await;
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).expect("a JSON answer")
}

fn titles(answer: &Value) -> Vec<String> {
    answer["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|result| result["title"].as_str().expect("title").to_string())
        .collect()
}

#[tokio::test]
async fn search_returns_only_pages_the_reader_may_open() {
    let idp = MockIdp::start().await;
    let qmd = MockQmd::start().await;
    let server = TestServer::start_with_search(&sites(), Some(&idp), &qmd.base_url).await;
    qmd.rank(EVERYTHING);

    let team = sign_in(&server, &idp, "team@corp.com").await;
    let answer = results(&server, Some(&team), "laptop").await;
    assert_eq!(titles(&answer), ["Onboarding", "My Notes", "Handbook"]);
    let raw = answer.to_string();
    for secret in [SECRET_TITLE, SECRET_BODY, "salaries", "People Ops", "Plans"] {
        assert!(
            !raw.contains(secret),
            "{secret} reached a reader without access: {raw}"
        );
    }
    let (_, page) = fetch(&server, Some(&team), "/__search?q=laptop").await;
    for secret in [
        SECRET_TITLE,
        SECRET_BODY,
        "salaries",
        "People Ops",
        "Unpublished",
    ] {
        assert!(!page.contains(secret), "{secret} reached the results page");
    }

    // The same ranking, for a reader who may open the restricted folder.
    let hr = sign_in(&server, &idp, "hr@corp.com").await;
    let answer = results(&server, Some(&hr), "laptop").await;
    assert_eq!(
        titles(&answer),
        [
            SECRET_TITLE,
            "People Ops",
            "Onboarding",
            "My Notes",
            "Handbook"
        ]
    );
}

/// A query whose matches are all restricted must not answer differently from a query
/// that matches nothing, or the index becomes a way to learn what exists.
#[tokio::test]
async fn a_ranking_of_restricted_pages_answers_like_a_ranking_of_nothing() {
    let idp = MockIdp::start().await;
    let qmd = MockQmd::start().await;
    let server = TestServer::start_with_search(&sites(), Some(&idp), &qmd.base_url).await;
    let team = sign_in(&server, &idp, "team@corp.com").await;

    for path in ["/__search?q=compensation", "/__search.json?q=compensation"] {
        qmd.rank(&[
            "qmd://docs/hr/salaries.md",
            "qmd://docs/plans.md",
            "qmd://docs/hr/index.md",
        ]);
        let restricted = fetch(&server, Some(&team), path).await;
        qmd.rank(&[]);
        let nothing = fetch(&server, Some(&team), path).await;
        assert_eq!(restricted, nothing, "{path}");
    }
}

#[tokio::test]
async fn results_and_snippets_never_carry_access_rules() {
    let idp = MockIdp::start().await;
    let qmd = MockQmd::start().await;
    let server = TestServer::start_with_search(&sites(), Some(&idp), &qmd.base_url).await;
    let team = sign_in(&server, &idp, "team@corp.com").await;
    qmd.rank(&["qmd://docs/index.md"]);

    let answer = results(&server, Some(&team), "hr corp handbook").await;
    assert_eq!(titles(&answer), ["Handbook"]);
    assert!(!answer.to_string().contains("hr@corp.com"), "{answer}");
    let snippet: String = answer["results"][0]["snippet"]
        .as_array()
        .expect("snippet")
        .iter()
        .map(|segment| segment["text"].as_str().expect("text"))
        .collect();
    assert_eq!(snippet, "The company handbook.");
    let marked: Vec<&str> = answer["results"][0]["snippet"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|segment| segment["match"] == true)
        .map(|segment| segment["text"].as_str().unwrap())
        .collect();
    assert_eq!(marked, ["handbook"]);
}

#[tokio::test]
async fn the_corpus_holds_published_bodies_and_follows_the_vault() {
    let qmd = MockQmd::start().await;
    let server = TestServer::start_with_search(&sites(), None, &qmd.base_url).await;
    let corpus = server.corpus().join("docs");

    assert_eq!(
        std::fs::read_to_string(corpus.join("onboarding.md")).unwrap(),
        "# Onboarding\n\nWelcome aboard. Laptops arrive on day one.\n"
    );
    // A page whose title lives only in frontmatter keeps it searchable.
    assert_eq!(
        std::fs::read_to_string(corpus.join("Odd Dir/My Notes.md")).unwrap(),
        "# My Notes\n\nNotes about laptop stickers.\n"
    );
    assert!(!corpus.join("plans.md").exists(), "a draft was indexed");
    for entry in walk(&server.corpus()) {
        let text = std::fs::read_to_string(&entry).unwrap();
        assert!(
            !text.contains("allow") && !text.contains("deny") && !text.contains("@corp.com"),
            "{} carries access rules: {text}",
            entry.display()
        );
    }

    server.remove_and_rebuild("onboarding.md").await;
    assert!(
        !corpus.join("onboarding.md").exists(),
        "a deleted page stayed indexed"
    );
    server
        .write_and_rebuild("guides/new.md", "---\ntitle: New\n---\n\n# New\n\nFresh.\n")
        .await;
    assert_eq!(
        std::fs::read_to_string(corpus.join("guides/new.md")).unwrap(),
        "# New\n\nFresh.\n"
    );
    server.remove_and_rebuild("guides/new.md").await;
    assert!(
        !corpus.join("guides").exists(),
        "an emptied folder was left behind"
    );
}

#[tokio::test]
async fn anonymous_visitors_get_the_sign_in_page_from_search() {
    let idp = MockIdp::start().await;
    let qmd = MockQmd::start().await;
    let server = TestServer::start_with_search(&sites(), Some(&idp), &qmd.base_url).await;
    qmd.rank(EVERYTHING);

    let interstitial = fetch(&server, None, "/docs/onboarding").await;
    assert_eq!(interstitial.0, 401);
    for path in ["/__search?q=laptop", "/__search.json?q=laptop"] {
        assert_eq!(fetch(&server, None, path).await, interstitial, "{path}");
    }
    assert_eq!(qmd.queries(), 0, "an anonymous request reached the index");
}

#[tokio::test]
async fn search_exists_only_where_it_is_configured() {
    let without = TestServer::start_public(VAULT).await;
    let unknown = fetch(&without, None, "/__nothing-here").await;
    assert_eq!(fetch(&without, None, "/__search?q=laptop").await, unknown);
    assert_eq!(
        fetch(&without, None, "/__search.json?q=laptop").await,
        unknown
    );
    assert!(
        !fetch(&without, None, "/docs/onboarding")
            .await
            .1
            .contains("search-open")
    );

    let qmd = MockQmd::start().await;
    let with = TestServer::start_with_search(&sites(), None, &qmd.base_url).await;
    assert!(
        fetch(&with, None, "/docs/onboarding")
            .await
            .1
            .contains("id=\"search-open\"")
    );
}

#[tokio::test]
async fn a_share_link_reader_is_offered_no_search() {
    let idp = MockIdp::start().await;
    let qmd = MockQmd::start().await;
    let server = TestServer::start_with_search(&sites(), Some(&idp), &qmd.base_url).await;
    let token = server.mint_link("onboarding.md", "team@corp.com", 60_000);
    let (status, page) = fetch(&server, None, &format!("/s/{token}")).await;
    assert_eq!(status, 200);
    assert!(
        !page.contains("search-open") && !page.contains("/__search"),
        "{page}"
    );
}

#[tokio::test]
async fn an_unreachable_index_is_reported_not_presented_as_no_matches() {
    let qmd = MockQmd::start().await;
    let server = TestServer::start_with_search(&sites(), None, &qmd.base_url).await;
    qmd.fail();

    let answer = results(&server, None, "laptop").await;
    assert_eq!(answer["unavailable"], true);
    assert_eq!(titles(&answer), Vec::<String>::new());
    let (status, page) = fetch(&server, None, "/__search?q=laptop").await;
    assert_eq!(status, 200);
    assert!(page.contains("Search is unavailable right now"), "{page}");
    assert!(!page.contains("No pages match"), "{page}");
}

#[tokio::test]
async fn an_empty_query_does_not_reach_the_index() {
    let qmd = MockQmd::start().await;
    let server = TestServer::start_with_search(&sites(), None, &qmd.base_url).await;
    let answer = results(&server, None, "   ").await;
    assert_eq!(titles(&answer), Vec::<String>::new());
    assert_eq!(answer["unavailable"], false);
    assert_eq!(qmd.queries(), 0);
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}
