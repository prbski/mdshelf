//! `mdshelf auth setup` — the guided walkthrough for creating a Google OAuth client.
//!
//! Existing to close the gap D14 opens: bring-your-own credentials keeps secrets out of
//! the binary, but leaves the operator in the Google Cloud Console with a form and no
//! idea which redirect URI mdshelf will actually use. The wizard supplies the exact
//! value, takes the credentials, and proves they work before it exits — so the first
//! real sign-in is not the first time anything is tested.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::{CLIENT_ID_ENV, CLIENT_SECRET_ENV, Credentials, credentials_file, crypto, oidc};

/// Run the wizard.
pub async fn run(public_url: &str, self_signed: Option<&PathBuf>) -> Result<()> {
    if let Some(directory) = self_signed {
        return generate_self_signed(directory, public_url);
    }

    let redirect_uri = format!("{}/auth/callback", public_url.trim_end_matches('/'));

    println!("Set up Google sign-in for mdshelf");
    println!();
    println!("  1. Open https://console.cloud.google.com/projectcreate and create a project");
    println!("     (or pick an existing one).");
    println!();
    println!("  2. Go to APIs & Services → OAuth consent screen.");
    println!("     • Google Workspace organisation → choose Internal. Nobody outside your");
    println!("       domain can sign in, and there is no verification review.");
    println!("     • Otherwise → choose External. While it stays in Testing you must list");
    println!("       each reader under Test users, at most 100, and their sessions expire");
    println!("       after 7 days. Click Publish to lift both limits.");
    println!();
    println!("  3. Go to APIs & Services → Credentials → Create credentials →");
    println!("     OAuth client ID → Web application.");
    println!();
    println!("  4. Under \"Authorised redirect URIs\", add exactly this:");
    println!();
    println!("         {redirect_uri}");
    println!();
    if copy_to_clipboard(&redirect_uri) {
        println!("     (copied to your clipboard)");
    }
    println!("     Google compares this string character for character. A trailing slash,");
    println!("     http instead of https, or a different port will all fail with");
    println!("     redirect_uri_mismatch.");
    println!();
    println!("  5. Copy the client ID and client secret it gives you, and paste them here.");
    println!();

    let client_id = prompt("Client ID: ")?;
    if client_id.is_empty() {
        bail!("no client ID entered; nothing was written");
    }
    let client_secret = prompt("Client secret: ")?;
    if client_secret.is_empty() {
        bail!("no client secret entered; nothing was written");
    }

    let credentials = Credentials {
        client_id: client_id.clone(),
        client_secret: client_secret.clone(),
    };

    let path = credentials_file()?;
    write_credentials(&path, &credentials)?;
    println!();
    println!("Wrote {} (mode 0600).", path.display());
    println!(
        "Environment variables {CLIENT_ID_ENV} / {CLIENT_SECRET_ENV} override this file \
         if you prefer a secret manager."
    );

    println!();
    match verify(&credentials, public_url, &redirect_uri).await {
        Ok(email) => {
            println!("✓ test sign-in succeeded as {email}");
            println!();
            println!("Start the server with:");
            println!("    mdshelf serve --auth google");
            Ok(())
        }
        Err(error) => {
            // The credentials are already saved, so the user can fix the console side
            // and re-verify rather than retyping everything.
            println!("✗ test sign-in did not complete");
            println!();
            println!("{error:#}");
            println!();
            println!("Common causes:");
            println!("  • The redirect URI in the console does not match {redirect_uri} exactly.");
            println!("  • The consent screen is still in Testing and your address is not");
            println!("    listed under Test users.");
            println!("  • The client secret was truncated when pasted.");
            bail!("setup incomplete")
        }
    }
}

/// Drive one real sign-in against Google to prove the configuration works.
async fn verify(credentials: &Credentials, public_url: &str, redirect_uri: &str) -> Result<String> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let discovery = std::env::var(super::DISCOVERY_URL_ENV)
        .unwrap_or_else(|_| oidc::GOOGLE_DISCOVERY_URL.to_string());
    let provider = oidc::Provider::discover(http, &discovery)
        .await
        .context("reaching Google's OpenID configuration")?;

    let verifier = crypto::random_token(48);
    let challenge = crypto::pkce_challenge(&verifier);
    let nonce = crypto::random_token(24);
    let state = crypto::random_token(32);

    let authorize_url = provider.authorization_url(
        &credentials.client_id,
        redirect_uri,
        &state,
        &nonce,
        &challenge,
    );

    let port = port_of(public_url).unwrap_or(80);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("binding 127.0.0.1:{port} to receive the test callback"))?;

    println!("Open this URL to complete the test sign-in:");
    println!();
    println!("    {authorize_url}");
    println!();
    println!("Waiting for the callback (Ctrl-C to skip)…");

    let (code, returned_state) =
        tokio::time::timeout(Duration::from_secs(300), wait_for_callback(listener))
            .await
            .map_err(|_| anyhow::anyhow!("timed out after 5 minutes waiting for the callback"))??;

    if returned_state != state {
        bail!("the callback carried an unexpected `state` value");
    }

    let tokens = provider
        .exchange_code(
            &credentials.client_id,
            &credentials.client_secret,
            redirect_uri,
            &code,
            &verifier,
        )
        .await
        .context("exchanging the authorization code")?;

    let id_token = tokens
        .id_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Google returned no ID token"))?;
    let identity = provider
        .verify_id_token(id_token, &credentials.client_id, Some(&nonce))
        .await
        .context("verifying the ID token")?;

    if tokens.refresh_token.is_none() {
        // Without one, sessions cannot be re-validated against Google (D18).
        println!(
            "  note: Google returned no refresh token. Revoke mdshelf's access at \
             https://myaccount.google.com/permissions and sign in again if sessions \
             fail to re-validate."
        );
    }

    Ok(identity.email)
}

/// Accept one HTTP request, pull `code` and `state` from it, and answer the browser.
async fn wait_for_callback(listener: tokio::net::TcpListener) -> Result<(String, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buffer = vec![0u8; 8192];
        let read = stream.read(&mut buffer).await?;
        let request = String::from_utf8_lossy(&buffer[..read]);

        let Some(target) = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
        else {
            continue;
        };
        if !target.starts_with("/auth/callback") {
            let _ = stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
            continue;
        }

        let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
        let mut code = None;
        let mut state = None;
        let mut error = None;
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => state = Some(value.into_owned()),
                "error" => error = Some(value.into_owned()),
                _ => {}
            }
        }

        let body = if error.is_some() {
            "<h1>Sign-in was declined</h1><p>You can close this tab and return to the terminal.</p>"
        } else {
            "<h1>mdshelf is set up</h1><p>You can close this tab and return to the terminal.</p>"
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.flush().await;

        if let Some(error) = error {
            bail!("Google reported: {error}");
        }
        match (code, state) {
            (Some(code), Some(state)) => return Ok((code, state)),
            _ => bail!("the callback was missing `code` or `state`"),
        }
    }
}

fn write_credentials(path: &PathBuf, credentials: &Credentials) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let body = format!(
        "# Written by `mdshelf auth setup`. Treat this file as a secret.\n\
         {CLIENT_ID_ENV}={}\n{CLIENT_SECRET_ENV}={}\n",
        credentials.client_id, credentials.client_secret
    );
    write_owner_only(path, body.as_bytes()).with_context(|| format!("writing {}", path.display()))
}

#[cfg(unix)]
fn write_owner_only(path: &PathBuf, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    // Truncate rather than create_new so re-running the wizard replaces the file.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    // An existing file keeps its old mode through `open`, so set it explicitly.
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_owner_only(path: &PathBuf, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Generate a self-signed certificate for LAN or offline testing.
fn generate_self_signed(directory: &PathBuf, public_url: &str) -> Result<()> {
    let host = host_of(public_url).unwrap_or_else(|| "localhost".to_string());
    let certificate = rcgen::generate_simple_self_signed(vec![host.clone()])
        .context("generating a self-signed certificate")?;

    std::fs::create_dir_all(directory)
        .with_context(|| format!("creating {}", directory.display()))?;
    let cert_path = directory.join("mdshelf-dev.crt");
    let key_path = directory.join("mdshelf-dev.key");

    std::fs::write(&cert_path, certificate.cert.pem())?;
    write_owner_only(
        &key_path,
        certificate.signing_key.serialize_pem().as_bytes(),
    )?;

    println!("Wrote {} and {}", cert_path.display(), key_path.display());
    println!();
    println!("    mdshelf serve --auth google \\");
    println!("        --tls-cert {} \\", cert_path.display());
    println!("        --tls-key {}", key_path.display());
    println!();
    println!("This certificate is for pre-authentication testing on a LAN only.");
    println!("Browsers will warn about it, and Google will not accept a self-signed host");
    println!("as an OAuth redirect URI — for a real deployment use --domain or a");
    println!("certificate from a public CA.");
    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Best-effort clipboard copy. Never an error: it is a convenience, not a step.
fn copy_to_clipboard(text: &str) -> bool {
    let candidates: [(&str, &[&str]); 4] = [
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("clip", &[]),
    ];
    for (program, args) in candidates {
        let Ok(mut child) = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(stdin) = child.stdin.as_mut()
            && stdin.write_all(text.as_bytes()).is_ok()
        {
            drop(child.stdin.take());
            if child.wait().map(|status| status.success()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

fn port_of(url: &str) -> Option<u16> {
    url::Url::parse(url).ok().and_then(|parsed| {
        parsed.port().or(match parsed.scheme() {
            "https" => Some(443),
            "http" => Some(80),
            _ => None,
        })
    })
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ports_including_the_scheme_defaults() {
        assert_eq!(port_of("http://127.0.0.1:4444"), Some(4444));
        assert_eq!(port_of("https://docs.acme.com"), Some(443));
        assert_eq!(port_of("http://docs.acme.com"), Some(80));
    }

    #[test]
    fn extracts_hosts() {
        assert_eq!(
            host_of("https://docs.acme.com/x").as_deref(),
            Some("docs.acme.com")
        );
        assert_eq!(
            host_of("http://127.0.0.1:4444").as_deref(),
            Some("127.0.0.1")
        );
    }

    #[cfg(unix)]
    #[test]
    fn credentials_are_written_owner_only_and_are_readable_back() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.env");
        let credentials = Credentials {
            client_id: "id-123".into(),
            client_secret: "secret-456".into(),
        };

        write_credentials(&path, &credentials).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "a credentials file must not be readable by others"
        );

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("MDSHELF_GOOGLE_CLIENT_ID=id-123"));
        assert!(contents.contains("MDSHELF_GOOGLE_CLIENT_SECRET=secret-456"));
    }

    #[cfg(unix)]
    #[test]
    fn rewriting_credentials_restores_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.env");
        std::fs::write(&path, "stale").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_credentials(
            &path,
            &Credentials {
                client_id: "a".into(),
                client_secret: "b".into(),
            },
        )
        .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "re-running setup must not leave a loosened file in place"
        );
    }
}
