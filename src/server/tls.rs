//! TLS: deciding how the server is reachable, and refusing the unsafe combinations.
//!
//! Google permits a plain-HTTP OAuth redirect URI only for `localhost`, so a real
//! deployment needs a certificate whatever mdshelf would prefer. The three supported
//! routes are ACME for a public domain, a supplied certificate, and TLS terminated
//! upstream (D25).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::cli::ServeArgs;
use crate::config::Config;

/// How this server will be exposed.
pub enum TlsMode {
    /// Plain HTTP. Only reached when auth is off, or on a loopback bind.
    Plain,
    /// A certificate supplied by the operator.
    Supplied { cert: PathBuf, key: PathBuf },
    /// A certificate obtained and renewed via ACME.
    Acme(AcmeSettings),
    /// TLS terminated by something in front of mdshelf.
    BehindProxy,
}

pub struct AcmeSettings {
    pub domain: String,
    pub contact: Option<String>,
    pub cache_dir: PathBuf,
    pub staging: bool,
}

impl TlsMode {
    /// Whether browsers reach this server over HTTPS.
    pub fn is_https(&self) -> bool {
        !matches!(self, TlsMode::Plain)
    }

    pub fn describe(&self) -> &'static str {
        match self {
            TlsMode::Plain => "http (no TLS)",
            TlsMode::Supplied { .. } => "https (supplied certificate)",
            TlsMode::Acme(_) => "https (ACME)",
            TlsMode::BehindProxy => "https (terminated upstream)",
        }
    }
}

/// Work out the TLS mode, refusing combinations that would serve sessions in the clear.
pub fn resolve(config: &Config, args: &ServeArgs) -> Result<TlsMode> {
    if let (Some(cert), Some(key)) = (args.tls_cert.clone(), args.tls_key.clone()) {
        for path in [&cert, &key] {
            if !path.is_file() {
                bail!("TLS file {} does not exist", path.display());
            }
        }
        return Ok(TlsMode::Supplied { cert, key });
    }

    if let Some(domain) = args.domain.clone() {
        if domain.contains("://") || domain.contains('/') {
            bail!("--domain takes a bare hostname such as docs.acme.com, not a URL");
        }
        let cache_dir = match args.acme_cache.clone() {
            Some(dir) => dir,
            None => default_acme_cache()?,
        };
        return Ok(TlsMode::Acme(AcmeSettings {
            domain,
            contact: args.acme_contact.clone(),
            cache_dir,
            staging: args.acme_staging,
        }));
    }

    if args.behind_proxy {
        // `--behind-proxy` asserts that something upstream terminates TLS. An http://
        // public URL contradicts that assertion: browsers would carry session cookies
        // in the clear, and mdshelf would have no way to notice. Taking the operator's
        // word for it while their own flag says otherwise is not a kindness.
        //
        // Google would reject the redirect URI too, but at first sign-in rather than at
        // startup, which is a much worse place to discover it.
        if args.auth.is_some() {
            let public_url = args.public_url.as_deref().unwrap_or_default();
            if public_url.starts_with("http://") && !is_loopback_url(public_url) {
                bail!(
                    "--behind-proxy says TLS is terminated upstream, but --public-url is \
                     {public_url}.\n  \
                     Browsers would send session cookies over plain HTTP.\n  \
                     Use an https:// public URL, or terminate TLS in mdshelf with \
                     --domain or --tls-cert."
                );
            }
        }
        return Ok(TlsMode::BehindProxy);
    }

    // Nothing was requested. That is fine without auth, and fine on loopback, but
    // serving authenticated sessions in the clear on a public interface is not
    // something to warn about and then do anyway.
    if args.auth.is_some() && !is_loopback_host(&config.host) {
        bail!(
            "--auth google on {} would serve session cookies over plain HTTP.\n  \
             Choose one:\n    \
             --domain <host>            obtain a Let's Encrypt certificate automatically\n    \
             --tls-cert <f> --tls-key <f>  use a certificate you already have\n    \
             --behind-proxy --public-url https://…  TLS is terminated upstream\n  \
             (Google also requires an https:// redirect URI for anything but localhost.)",
            config.host
        );
    }

    Ok(TlsMode::Plain)
}

/// The origin browsers use, given the resolved TLS mode.
pub fn public_url(config: &Config, args: &ServeArgs, mode: &TlsMode, port: u16) -> Result<String> {
    if let Some(explicit) = args.public_url.as_deref() {
        let trimmed = explicit.trim_end_matches('/');
        if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
            bail!("--public-url must start with http:// or https://; got `{explicit}`");
        }
        return Ok(trimmed.to_string());
    }

    if let TlsMode::Acme(settings) = mode {
        // ACME serves on 443, and naming the port would break the exact-match redirect
        // URI Google requires.
        return Ok(format!("https://{}", settings.domain));
    }

    let host = config.host.as_str();
    if is_loopback_host(host) {
        let scheme = if mode.is_https() { "https" } else { "http" };
        return Ok(format!("{scheme}://{host}:{port}"));
    }

    bail!(
        "mdshelf needs to know the URL browsers use to reach this server, because Google \
         matches the OAuth redirect URI exactly.\n  Pass --public-url https://your.domain"
    )
}

/// True when a URL points at this machine, where plain HTTP is acceptable — and is the
/// one case Google exempts from its https-only redirect URI rule.
fn is_loopback_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    let authority = rest.split('/').next().unwrap_or_default();
    // A bracketed IPv6 literal is full of colons, so the port cannot be split off by
    // splitting on ':' — the host runs to the closing bracket.
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

pub fn is_loopback_host(host: &str) -> bool {
    matches!(
        host,
        "127.0.0.1" | "localhost" | "::1" | "[::1]" | "0.0.0.0"
    ) && host != "0.0.0.0"
}

fn default_acme_cache() -> Result<PathBuf> {
    Ok(crate::config::user_config_dir()
        .context("resolving the default ACME cache; pass --acme-cache explicitly")?
        .join("acme"))
}

/// Serve `router` under the resolved TLS mode.
pub async fn serve(mode: TlsMode, address: SocketAddr, router: axum::Router) -> Result<()> {
    match mode {
        TlsMode::Plain | TlsMode::BehindProxy => {
            let listener = tokio::net::TcpListener::bind(address).await?;
            axum::serve(listener, router).await?;
            Ok(())
        }
        TlsMode::Supplied { cert, key } => {
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                .await
                .with_context(|| {
                    format!(
                        "loading TLS certificate {} and key {}",
                        cert.display(),
                        key.display()
                    )
                })?;
            axum_server::bind_rustls(address, config)
                .serve(router.into_make_service())
                .await?;
            Ok(())
        }
        TlsMode::Acme(settings) => serve_acme(settings, router).await,
    }
}

async fn serve_acme(settings: AcmeSettings, router: axum::Router) -> Result<()> {
    use futures::StreamExt;
    use rustls_acme::AcmeConfig;
    use rustls_acme::caches::DirCache;

    std::fs::create_dir_all(&settings.cache_dir).with_context(|| {
        format!(
            "creating the ACME cache directory {}",
            settings.cache_dir.display()
        )
    })?;

    let mut state = AcmeConfig::new([settings.domain.as_str()])
        .contact_push(
            settings
                .contact
                .as_deref()
                .map(|address| format!("mailto:{address}"))
                .unwrap_or_default(),
        )
        .cache(DirCache::new(settings.cache_dir.clone()))
        .directory_lets_encrypt(!settings.staging)
        .state();

    let acceptor = state.axum_acceptor(state.default_rustls_config());

    tokio::spawn(async move {
        loop {
            match state.next().await {
                Some(Ok(event)) => tracing::info!(?event, "ACME event"),
                // An ACME failure leaves the previously cached certificate in place, so
                // it is logged loudly rather than taken as fatal to an already-running
                // server.
                Some(Err(error)) => tracing::error!(%error, "ACME error"),
                None => return,
            }
        }
    });

    // ACME's HTTP-01/TLS-ALPN-01 challenges are answered on 443, and Google requires a
    // redirect URI with no port, so this path always binds the standard HTTPS port.
    axum_server::bind(SocketAddr::from(([0, 0, 0, 0], 443)))
        .acceptor(acceptor)
        .serve(router.into_make_service())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_are_recognised() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));
        // 0.0.0.0 binds every interface, so it is emphatically not loopback.
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("docs.acme.com"));
        assert!(!is_loopback_host("192.168.1.10"));
    }

    /// Bare `ServeArgs`, then mutate the fields a case cares about.
    fn args() -> ServeArgs {
        ServeArgs {
            config: None,
            host: None,
            port: None,
            no_live_reload: false,
            auth: None,
            public_url: None,
            domain: None,
            tls_cert: None,
            tls_key: None,
            behind_proxy: false,
            acme_contact: None,
            acme_cache: None,
            acme_staging: false,
        }
    }

    fn config_on(host: &str) -> Config {
        let dir = tempfile::tempdir().expect("temp dir");
        let content = dir.path().join("content");
        std::fs::create_dir_all(&content).unwrap();
        let mut config = Config::for_test(
            dir.path().to_path_buf(),
            vec![crate::config::SiteConfig::for_test(&content)],
        );
        config.host = host.to_string();
        // Leak the temp dir: the config borrows nothing from it, and the test only
        // needs the paths to have existed at construction.
        std::mem::forget(dir);
        config
    }

    /// The whole matrix in one place. Authenticated sessions must never be served
    /// where browsers would use plain HTTP off-loopback.
    #[test]
    fn authenticated_sessions_are_never_served_in_the_clear() {
        // Auth off: anything goes, exactly as before auth existed (NFR-2).
        assert!(resolve(&config_on("0.0.0.0"), &args()).is_ok());

        // Auth on, loopback, no TLS: fine, and the one case Google exempts.
        let mut a = args();
        a.auth = Some("google".into());
        assert!(resolve(&config_on("127.0.0.1"), &a).is_ok());
        assert!(resolve(&config_on("localhost"), &a).is_ok());

        // Auth on, public interface, no TLS of any kind: refused.
        for host in ["0.0.0.0", "192.168.1.10", "docs.acme.com", "::"] {
            let error = match resolve(&config_on(host), &a) {
                Ok(_) => panic!("{host} without TLS must be refused"),
                Err(error) => error.to_string(),
            };
            assert!(error.contains("plain HTTP"), "{host}: {error}");
        }

        // Auth on, behind a proxy, but the public URL is plain HTTP: refused, because
        // the flag's own claim is that browsers see HTTPS.
        let mut proxy = a.clone();
        proxy.behind_proxy = true;
        proxy.public_url = Some("http://docs.acme.com".into());
        let error = match resolve(&config_on("0.0.0.0"), &proxy) {
            Ok(_) => panic!("http:// behind a proxy must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("terminated upstream"), "got: {error}");

        // ...but https behind a proxy is the supported deployment.
        proxy.public_url = Some("https://docs.acme.com".into());
        assert!(matches!(
            resolve(&config_on("0.0.0.0"), &proxy),
            Ok(TlsMode::BehindProxy)
        ));

        // A proxy on the same machine during development stays workable.
        proxy.public_url = Some("http://localhost:8080".into());
        assert!(resolve(&config_on("127.0.0.1"), &proxy).is_ok());

        // ACME satisfies the requirement on any interface.
        let mut acme = a.clone();
        acme.domain = Some("docs.acme.com".into());
        assert!(matches!(
            resolve(&config_on("0.0.0.0"), &acme),
            Ok(TlsMode::Acme(_))
        ));

        // A URL passed where a hostname belongs is a mistake worth naming.
        acme.domain = Some("https://docs.acme.com".into());
        assert!(resolve(&config_on("0.0.0.0"), &acme).is_err());
    }

    #[test]
    fn loopback_urls_are_recognised_across_forms() {
        assert!(is_loopback_url("http://localhost:8080"));
        assert!(is_loopback_url("http://127.0.0.1:4444/x"));
        assert!(is_loopback_url("http://[::1]:4444"));
        assert!(!is_loopback_url("http://docs.acme.com"));
        // Not loopback just because the string contains it.
        assert!(!is_loopback_url("http://localhost.evil.example"));
        assert!(!is_loopback_url("http://127.0.0.1.evil.example"));
    }

    #[test]
    fn tls_modes_report_whether_browsers_see_https() {
        assert!(!TlsMode::Plain.is_https());
        assert!(TlsMode::BehindProxy.is_https());
        assert!(
            TlsMode::Supplied {
                cert: PathBuf::from("c"),
                key: PathBuf::from("k")
            }
            .is_https()
        );
    }
}
