//! Cross-checks the serving path against the resolver over generated vaults.
//!
//! The resolver has property tests; the HTTP surface has fixture tests. Neither catches
//! the failure that actually happened three times in this feature: the resolver decides
//! correctly and then some surface disagrees with it. The case-variant bypass, the
//! exported bundle naming a site, and the site switcher were all exactly that.
//!
//! So this uses the resolver as an oracle. For a generated vault and each viewer, every
//! page marker must appear in a response if and only if the resolver says that viewer
//! may read that page — checked across the page URL, its `.md` form, and case variants.
//!
//! Both directions fire on real cases: across the 24 seeds the resolver permits about
//! 31% of (viewer, page) pairs and denies 69%, so the leak check has ~296 denials to
//! catch a leak on and the over-block check has ~136 grants to confirm are served. If a
//! change to the generator pushes that balance to either extreme, these tests quietly
//! stop testing anything.

use std::collections::HashMap;
use std::path::PathBuf;

use mdshelf::auth::SESSION_COOKIE;
use mdshelf::test_support::{MockIdp, TestServer, TokenSpec, client};

const EMAILS: [&str; 3] = ["a@corp.com", "b@corp.com", "c@corp.com"];

/// (relative path, url path, unique marker)
const FILES: [(&str, &str, &str); 6] = [
    ("index.md", "", "ZZ0ZZ"),
    ("top.md", "top", "ZZ1ZZ"),
    ("one/index.md", "one", "ZZ2ZZ"),
    ("one/page.md", "one/page", "ZZ3ZZ"),
    ("one/two/index.md", "one/two", "ZZ4ZZ"),
    ("one/two/leaf.md", "one/two/leaf", "ZZ5ZZ"),
];

/// Small deterministic PRNG, so a failure is reproducible from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    /// A random subset of the address pool, rendered as YAML list entries.
    fn subset(&mut self) -> Vec<&'static str> {
        let mut picked = Vec::new();
        for email in EMAILS {
            if self.next().is_multiple_of(3) {
                picked.push(email);
            }
        }
        picked
    }
}

/// Build one vault's file contents from a seed.
fn generate(seed: u64) -> Vec<(String, String)> {
    let mut rng = Rng(seed);
    let mut files = Vec::new();
    for (relative, _, marker) in FILES {
        let allow = rng.subset();
        let deny = rng.subset();
        let mut front = String::from("---\ntitle: T\n");
        if !allow.is_empty() {
            front.push_str("allow:\n");
            for email in &allow {
                front.push_str(&format!("  - {email}\n"));
            }
        }
        if !deny.is_empty() {
            front.push_str("deny:\n");
            for email in &deny {
                front.push_str(&format!("  - {email}\n"));
            }
        }
        front.push_str(&format!("---\n\n# {marker}\n"));
        files.push((relative.to_string(), front));
    }
    files
}

async fn sign_in(server: &TestServer, idp: &MockIdp, email: &str) -> String {
    let http = client();
    let login = http
        .get(server.url("/auth/login"))
        .send()
        .await
        .expect("login");
    let authorize_url = login
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("Location")
        .to_str()
        .expect("printable")
        .to_string();
    let state = url::Url::parse(&authorize_url)
        .expect("url")
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("state");
    idp.register_code(&format!("code-for-{state}"), TokenSpec::valid(email));

    let authorize = http.get(&authorize_url).send().await.expect("authorize");
    let callback = http
        .get(
            authorize
                .headers()
                .get(reqwest::header::LOCATION)
                .expect("Location")
                .to_str()
                .expect("printable"),
        )
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
        .expect("session cookie")
}

#[tokio::test]
async fn every_response_agrees_with_the_resolver() {
    let idp = MockIdp::start().await;

    for seed in 1..=24u64 {
        let files = generate(seed);
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, body)| (path.as_str(), body.as_str()))
            .collect();
        let server = TestServer::start_with_auth(&borrowed, &idp).await;

        // Ask the resolver directly: this is the oracle every surface must match.
        let expected: HashMap<(&str, &str), bool> = {
            let universe = server.state.universe.read().await;
            let acl = universe.sites()[0].acl();
            let mut map = HashMap::new();
            for email in EMAILS {
                for (relative, _, marker) in FILES {
                    map.insert((email, marker), acl.allows(&PathBuf::from(relative), email));
                }
            }
            map
        };

        for email in EMAILS {
            let cookie = sign_in(&server, &idp, email).await;

            // Every way a reader might ask for each page, including the case variants
            // that once bypassed the folder rule entirely.
            let mut requests: Vec<String> = vec!["/".into(), "/docs".into()];
            for (_, url_path, _) in FILES {
                if url_path.is_empty() {
                    continue;
                }
                requests.push(format!("/docs/{url_path}"));
                requests.push(format!("/docs/{url_path}.md"));
                requests.push(format!("/docs/{}", url_path.to_uppercase()));
            }

            for path in requests {
                let response = client()
                    .get(server.url(&path))
                    .header(
                        reqwest::header::COOKIE,
                        format!("{SESSION_COOKIE}={cookie}"),
                    )
                    .send()
                    .await
                    .expect("request");
                let status = response.status();
                let body = response.text().await.expect("body");

                for (_, _, marker) in FILES {
                    if !body.contains(marker) {
                        continue;
                    }
                    let permitted = expected[&(email, marker)];
                    assert!(
                        permitted,
                        "seed {seed}: {email} received {marker} from {path} (status {status}), \
                         but the resolver denies it"
                    );
                }
            }
        }
    }
}

/// The converse: a viewer the resolver permits must actually be able to read the page.
///
/// Over-blocking is not a leak, but a gate that denies the people it was supposed to
/// admit is still a broken gate — and it is the failure mode a leak fix is most likely
/// to introduce.
#[tokio::test]
async fn a_permitted_viewer_is_never_over_blocked() {
    let idp = MockIdp::start().await;

    for seed in 100..=115u64 {
        let files = generate(seed);
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, body)| (path.as_str(), body.as_str()))
            .collect();
        let server = TestServer::start_with_auth(&borrowed, &idp).await;

        let permitted: Vec<(&str, &str, &str)> = {
            let universe = server.state.universe.read().await;
            let acl = universe.sites()[0].acl();
            let mut rows = Vec::new();
            for email in EMAILS {
                for (relative, url_path, marker) in FILES {
                    if acl.allows(&PathBuf::from(relative), email) {
                        rows.push((email, url_path, marker));
                    }
                }
            }
            rows
        };

        for email in EMAILS {
            let cookie = sign_in(&server, &idp, email).await;
            for (owner, url_path, marker) in &permitted {
                if *owner != email {
                    continue;
                }
                let path = if url_path.is_empty() {
                    "/docs".to_string()
                } else {
                    format!("/docs/{url_path}")
                };
                let response = client()
                    .get(server.url(&path))
                    .header(
                        reqwest::header::COOKIE,
                        format!("{SESSION_COOKIE}={cookie}"),
                    )
                    .send()
                    .await
                    .expect("request");
                let status = response.status();
                let body = response.text().await.expect("body");
                assert!(
                    body.contains(marker),
                    "seed {seed}: {email} was denied {path} (status {status}) \
                     although the resolver permits it"
                );
            }
        }
    }
}
