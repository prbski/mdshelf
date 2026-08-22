use std::ffi::OsString;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use service_manager::{
    ServiceInstallCtx, ServiceLabel, ServiceLevel, ServiceManager, ServiceStartCtx, ServiceStatus,
    ServiceStatusCtx, ServiceStopCtx, ServiceUninstallCtx,
};

use crate::cli::{ServiceArgs, ServiceNameArgs};
use crate::config::Config;

pub fn install(args: ServiceArgs) -> Result<()> {
    let config_path = Config::resolve_path(args.config.as_deref())?;
    let _loaded = Config::load(Some(&config_path))?;

    let mut manager = <dyn ServiceManager>::native().context("detect native service manager")?;
    if args.user {
        manager
            .set_level(ServiceLevel::User)
            .context("switch service manager to user scope")?;
    } else {
        manager
            .set_level(ServiceLevel::System)
            .context("switch service manager to system scope")?;
    }

    let label = service_label(&args.name)?;
    let executable_path = resolve_executable_path()?;

    let install_context = ServiceInstallCtx {
        label,
        program: executable_path,
        args: vec![
            OsString::from("serve"),
            OsString::from("--config"),
            config_path.as_os_str().to_owned(),
        ],
        contents: None,
        username: None,
        working_directory: None,
        environment: None,
        autostart: true,
        restart_policy: service_manager::RestartPolicy::default(),
    };

    manager
        .install(install_context)
        .map_err(|err| anyhow!("install service: {err}"))?;
    println!("installed service {}", args.name);
    Ok(())
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
