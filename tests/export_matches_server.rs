//! Does `export --as <email>` contain exactly what the server serves that person?
//!
//! Export is a third implementation of "what does this viewer see", alongside the
//! serving path and the resolver. The other two are already cross-checked against each
//! other; this closes the triangle.
//!
//! It matters because the two answers are consumed differently. A page the server
//! withholds but the export includes is a leak posted to a client; a page the server
//! serves but the export omits is a deliverable quietly missing content. Only a
//! comparison catches either.

use std::path::Path;

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

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn subset(&mut self) -> Vec<&'static str> {
        EMAILS
            .into_iter()
            .filter(|_| self.next().is_multiple_of(3))
            .collect()
    }
}

fn generate(seed: u64) -> Vec<(String, String)> {
    let mut rng = Rng(seed);
    FILES
        .iter()
        .map(|(relative, _, marker)| {
            let (allow, deny) = (rng.subset(), rng.subset());
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
            (relative.to_string(), front)
        })
        .collect()
}

fn scaffold(dir: &Path, files: &[(String, String)]) -> std::path::PathBuf {
    let vault = dir.join("vault");
    for (relative, contents) in files {
        let path = vault.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let config = dir.join("mdshelf.toml");
    std::fs::write(
        &config,
        "[[sites]]\npath = \"vault\"\nmount = \"/docs\"\ntitle = \"Docs\"\n",
    )
    .unwrap();
    config
}

/// Everything the bundle contains, concatenated for a single search.
fn bundle_text(root: &Path) -> String {
    let mut combined = String::new();
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if entry.file_type().is_file()
            && let Ok(text) = std::fs::read_to_string(entry.path())
        {
            combined.push_str(&text);
        }
    }
    combined
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
async fn an_export_contains_exactly_what_the_server_serves_that_viewer() {
    let idp = MockIdp::start().await;
    // Guards against the comparison passing because both sides are always empty.
    let mut total_served = 0usize;
    let mut viewers_with_access = 0usize;

    for seed in 1..=12u64 {
        let files = generate(seed);
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, body)| (path.as_str(), body.as_str()))
            .collect();
        let server = TestServer::start_with_auth(&borrowed, &idp).await;

        // The same vault on disk, for the exporter.
        let dir = tempfile::tempdir().expect("temp dir");
        let config = scaffold(dir.path(), &files);

        for email in EMAILS {
            let cookie = sign_in(&server, &idp, email).await;

            // What the server actually serves this viewer.
            let mut served = Vec::new();
            for (_, url_path, marker) in FILES {
                let path = if url_path.is_empty() {
                    "/docs".to_string()
                } else {
                    format!("/docs/{url_path}")
                };
                let body = client()
                    .get(server.url(&path))
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
                if body.contains(marker) {
                    served.push(marker);
                }
            }

            // What the exporter gives them.
            let out = dir.path().join(format!("out-{email}-{seed}"));
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_mdshelf"))
                .args([
                    "export",
                    "--config",
                    config.to_str().expect("path"),
                    "--output",
                    out.to_str().expect("path"),
                    "--as",
                    email,
                ])
                .output()
                .expect("running export");
            assert!(
                output.status.success(),
                "seed {seed}: export failed for {email}:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );

            let bundle = bundle_text(&out);
            let exported: Vec<&str> = FILES
                .iter()
                .filter(|(_, _, marker)| bundle.contains(marker))
                .map(|(_, _, marker)| *marker)
                .collect();

            assert_eq!(
                exported, served,
                "seed {seed}: export and server disagree for {email}\n  \
                 exported: {exported:?}\n  served:   {served:?}"
            );

            total_served += served.len();
            if !served.is_empty() {
                viewers_with_access += 1;
            }
        }
    }

    // An equivalence test between two empty sets proves nothing. If a change to the
    // generator ever drives access to zero, this fails rather than passing silently.
    assert!(
        total_served > 20,
        "only {total_served} pages were served across the whole run — the comparison \
         is not exercising anything"
    );
    assert!(
        viewers_with_access > 10,
        "only {viewers_with_access} of 36 viewer runs saw any page at all"
    );
    println!("compared {total_served} served pages across {viewers_with_access} viewer runs");
}
