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

pub fn is_loopback_host(host: &str) -> bool {
    matches!(
        host,
        "127.0.0.1" | "localhost" | "::1" | "[::1]" | "0.0.0.0"
    ) && host != "0.0.0.0"
}

fn default_acme_cache() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set; pass --acme-cache explicitly")?;
    Ok(home.join(".mdshelf/acme"))
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
