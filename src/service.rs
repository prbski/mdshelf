use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use service_manager::{
    ServiceInstallCtx, ServiceLabel, ServiceLevel, ServiceManager, ServiceStartCtx, ServiceStatus,
    ServiceStatusCtx, ServiceStopCtx, ServiceUninstallCtx,
};

use crate::auth::{AuthSettings, require_stored_credentials};
use crate::cli::{ServeArgs, ServiceArgs, ServiceNameArgs};
use crate::config::Config;
use crate::server::{self, tls};

pub fn install(args: ServiceArgs) -> Result<()> {
    let ServiceArgs {
        mut serve,
        name,
        user,
    } = args;
    serve.config = Some(Config::resolve_path(serve.config.as_deref())?);
    let plan = plan(&serve)?;

    let mut manager = <dyn ServiceManager>::native().context("detect native service manager")?;
    if user {
        manager
            .set_level(ServiceLevel::User)
            .context("switch service manager to user scope")?;
    } else {
        manager
            .set_level(ServiceLevel::System)
            .context("switch service manager to system scope")?;
    }

    let label = service_label(&name)?;
    let executable_path = resolve_executable_path()?;

    let install_context = ServiceInstallCtx {
        label,
        program: executable_path,
        args: serve_command(&serve)?,
        contents: None,
        username: None,
        working_directory: None,
        environment: plan.environment,
        autostart: true,
        restart_policy: service_manager::RestartPolicy::default(),
    };

    manager
        .install(install_context)
        .map_err(|err| anyhow!("install service: {err}"))?;
    println!("installed service {name}");
    if let Some(redirect_uri) = plan.redirect_uri {
        println!("register this redirect URI with your Google OAuth client: {redirect_uri}");
    }
    Ok(())
}

/// What the service needs beyond its command line.
struct Plan {
    environment: Option<Vec<(String, String)>>,
    redirect_uri: Option<String>,
}

/// Run the checks `serve` runs before it binds, so a service that could never start is
/// never registered.
///
/// Google credentials, the at-rest key and the ACME cache default to paths under
/// `$HOME`, which a system service does not have. The service gets the installing
/// user's `HOME`, so it resolves exactly the paths validated here.
fn plan(serve: &ServeArgs) -> Result<Plan> {
    let config = server::load_config(serve)?;
    let tls_mode = tls::resolve(&config, serve)?;
    let public_url = tls::public_url(&config, serve, &tls_mode, config.port)?;

    let redirect_uri = match serve.auth.as_deref() {
        Some(provider) => {
            server::check_auth_provider(provider)?;
            config.validate_link_prefix()?;
            require_stored_credentials()?;
            let auth = config.auth.clone().unwrap_or_default();
            Some(AuthSettings::resolve(&config, &auth, public_url)?.redirect_uri())
        }
        None => None,
    };

    let needs_home = serve.auth.is_some() || (serve.domain.is_some() && serve.acme_cache.is_none());
    if !needs_home {
        return Ok(Plan {
            environment: None,
            redirect_uri,
        });
    }
    if cfg!(windows) {
        bail!(
            "Windows services cannot be given HOME, which `--auth` and `--domain` need to find \
             credentials, the at-rest key and the ACME cache.\n  \
             Run `mdshelf serve` with those flags under a service wrapper instead."
        );
    }
    let home = std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .context("HOME is not set; the service needs it to find credentials and keys")?;
    Ok(Plan {
        environment: Some(vec![("HOME".to_string(), home)]),
        redirect_uri,
    })
}

/// `serve` and its flags, rebuilt from the parsed arguments with every path made
/// absolute: the service starts in a working directory of the service manager's choosing.
///
/// The destructuring is exhaustive on purpose, so a new `serve` flag fails to compile
/// here until the service learns to pass it on.
fn serve_command(serve: &ServeArgs) -> Result<Vec<OsString>> {
    let ServeArgs {
        config,
        host,
        port,
        no_live_reload,
        auth,
        public_url,
        domain,
        tls_cert,
        tls_key,
        behind_proxy,
        acme_contact,
        acme_cache,
        acme_staging,
    } = serve;

    let mut command = vec![OsString::from("serve")];
    let mut push = |flag: &str, value: OsString| {
        command.push(OsString::from(flag));
        command.push(value);
    };
    if let Some(path) = config {
        push("--config", absolute(path)?.into());
    }
    if let Some(host) = host {
        push("--host", host.into());
    }
    if let Some(port) = port {
        push("--port", port.to_string().into());
    }
    if let Some(provider) = auth {
        push("--auth", provider.into());
    }
    if let Some(url) = public_url {
        push("--public-url", url.into());
    }
    if let Some(domain) = domain {
        push("--domain", domain.into());
    }
    if let Some(path) = tls_cert {
        push("--tls-cert", absolute(path)?.into());
    }
    if let Some(path) = tls_key {
        push("--tls-key", absolute(path)?.into());
    }
    if let Some(contact) = acme_contact {
        push("--acme-contact", contact.into());
    }
    if let Some(path) = acme_cache {
        push("--acme-cache", absolute(path)?.into());
    }
    for (set, flag) in [
        (*no_live_reload, "--no-live-reload"),
        (*behind_proxy, "--behind-proxy"),
        (*acme_staging, "--acme-staging"),
    ] {
        if set {
            command.push(OsString::from(flag));
        }
    }
    Ok(command)
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))
}

pub fn uninstall(args: ServiceNameArgs) -> Result<()> {
    let mut manager = <dyn ServiceManager>::native().context("detect native service manager")?;
    if args.user {
        manager.set_level(ServiceLevel::User)?;
    } else {
        manager.set_level(ServiceLevel::System)?;
    }
    let label = service_label(&args.name)?;
    manager
        .uninstall(ServiceUninstallCtx {
            label: label.clone(),
        })
        .map_err(|err| anyhow!("uninstall service: {err}"))?;
    println!("uninstalled service {}", args.name);
    Ok(())
}

pub fn start(args: ServiceNameArgs) -> Result<()> {
    let mut manager = <dyn ServiceManager>::native().context("detect native service manager")?;
    if args.user {
        manager.set_level(ServiceLevel::User)?;
    } else {
        manager.set_level(ServiceLevel::System)?;
    }
    let label = service_label(&args.name)?;
    manager
        .start(ServiceStartCtx {
            label: label.clone(),
        })
        .map_err(|err| anyhow!("start service: {err}"))?;
    println!("started service {}", args.name);
    Ok(())
}

pub fn stop(args: ServiceNameArgs) -> Result<()> {
    let mut manager = <dyn ServiceManager>::native().context("detect native service manager")?;
    if args.user {
        manager.set_level(ServiceLevel::User)?;
    } else {
        manager.set_level(ServiceLevel::System)?;
    }
    let label = service_label(&args.name)?;
    manager
        .stop(ServiceStopCtx {
            label: label.clone(),
        })
        .map_err(|err| anyhow!("stop service: {err}"))?;
    println!("stopped service {}", args.name);
    Ok(())
}

pub fn restart(args: ServiceNameArgs) -> Result<()> {
    stop(args.clone())?;
    start(args)?;
    Ok(())
}

pub fn status(args: ServiceNameArgs) -> Result<()> {
    let mut manager = <dyn ServiceManager>::native().context("detect native service manager")?;
    if args.user {
        manager.set_level(ServiceLevel::User)?;
    } else {
        manager.set_level(ServiceLevel::System)?;
    }
    let label = service_label(&args.name)?;
    let status = manager
        .status(ServiceStatusCtx {
            label: label.clone(),
        })
        .map_err(|err| anyhow!("service status: {err}"))?;
    match status {
        ServiceStatus::NotInstalled => println!("{}: not installed", args.name),
        ServiceStatus::Running => println!("{}: running", args.name),
        ServiceStatus::Stopped(reason) => {
            if let Some(message) = reason {
                println!("{}: stopped ({})", args.name, message);
            } else {
                println!("{}: stopped", args.name);
            }
        }
    }
    Ok(())
}

/// Returns the path to the running executable without resolving symlinks.
///
/// `std::env::current_exe()` calls `realpath` on macOS, which resolves Homebrew
/// symlinks to versioned Cellar paths. Using argv[0] preserves the stable
/// PATH entry (e.g. `/opt/homebrew/bin/mdshelf`) so the service automatically
/// picks up upgrades on restart without needing to be reinstalled.
fn resolve_executable_path() -> Result<PathBuf> {
    let arg0 = std::env::args_os().next().context("argv[0] is missing")?;
    let path = PathBuf::from(&arg0);

    if path.is_absolute() {
        return Ok(path);
    }

    if path.components().count() > 1 {
        let cwd = std::env::current_dir().context("get current working directory")?;
        return Ok(cwd.join(path));
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path_var) {
            let candidate = directory.join(&path);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    std::env::current_exe().context("resolve path to mdshelf executable")
}

fn service_label(name: &str) -> Result<ServiceLabel> {
    let application: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    let qualified = format!("com.mdshelf.{application}");
    ServiceLabel::from_str(&qualified).map_err(|err| anyhow!("invalid service label: {err}"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn install_args(flags: &[&str]) -> ServeArgs {
        let argv = ["mdshelf", "install"].iter().chain(flags);
        match Cli::try_parse_from(argv).expect("install parses").command {
            Command::Install(args) => args.serve,
            other => panic!("parsed as {other:?}"),
        }
    }

    fn serve_args(command: &[OsString]) -> ServeArgs {
        let argv = std::iter::once(OsString::from("mdshelf")).chain(command.iter().cloned());
        match Cli::try_parse_from(argv)
            .expect("service command parses")
            .command
        {
            Command::Serve(args) => args,
            other => panic!("parsed as {other:?}"),
        }
    }

    /// The service must start with exactly the options it was installed with: every
    /// flag survives, and relative paths are pinned to the directory `install` ran in.
    #[test]
    fn the_service_runs_serve_with_every_install_option() {
        let cases: [&[&str]; 2] = [
            &[
                "--config",
                "site.toml",
                "--host",
                "0.0.0.0",
                "--port",
                "8443",
                "--no-live-reload",
                "--auth",
                "google",
                "--public-url",
                "https://docs.acme.com",
                "--behind-proxy",
                "--tls-cert",
                "certs/full.pem",
                "--tls-key",
                "certs/key.pem",
            ],
            &[
                "--domain",
                "docs.acme.com",
                "--acme-contact",
                "ops@acme.com",
                "--acme-cache",
                "acme",
                "--acme-staging",
            ],
        ];
        for flags in cases {
            let installed = install_args(flags);
            let mut expected = installed.clone();
            for path in [
                &mut expected.config,
                &mut expected.tls_cert,
                &mut expected.tls_key,
                &mut expected.acme_cache,
            ]
            .into_iter()
            .flatten()
            {
                *path = std::path::absolute(&*path).unwrap();
            }

            let started = serve_args(&serve_command(&installed).expect("command builds"));
            assert_eq!(format!("{started:?}"), format!("{expected:?}"));
        }
    }

    #[test]
    fn a_plain_install_runs_serve_with_only_its_config() {
        let command = serve_command(&install_args(&["--config", "/srv/site.toml"])).unwrap();
        assert_eq!(
            command,
            ["serve", "--config", "/srv/site.toml"].map(OsString::from)
        );
    }
}
