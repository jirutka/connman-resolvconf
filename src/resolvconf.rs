use std::env;
use std::fs;
use std::io::prelude::*;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use log::{debug, trace};
use nix::unistd::Uid;


const DEFAULT_RESOLVCONF: &str = "/usr/sbin/resolvconf";

/// Interface for calling `resolvconf(8)` command.
#[derive(Clone)]
pub struct Resolvconf {
    path: String,
}

impl Resolvconf {
    pub fn new() -> Resolvconf {
        Resolvconf {
            path: env::var("RESOLVCONF").unwrap_or_else(|_| DEFAULT_RESOLVCONF.into()),
        }
    }

    /// Adds DNS information to the specified interface (in resolv.conf format).
    pub fn add(&self, interface: &str, content: &str) -> Result<()> {
        check_permissions(&self.path)?;

        debug!("Executing command: {} -a {}", self.path, interface);
        let mut child = Command::new(&self.path)
            .args(["-a", interface])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .context(format!("Failed to execute {}", &self.path))?;

        let mut stdin = child.stdin.take().unwrap(); // this should never fail
        trace!("Writing to stdin pipe: \"{}\"", content);
        stdin
            .write_all(content.as_bytes())
            .context(format!("Error writing data to stdin pipe of {}", self.path))?;
        drop(stdin);

        let status = child
            .wait()
            .context(format!("Failed to execute {}", &self.path))?;

        if !status.success() {
            bail!("Command {} exited with {}", &self.path, status)
        }
        Ok(())
    }

    /// Deletes DNS information from the specified interface.
    pub fn del(&self, interface: &str) -> Result<()> {
        check_permissions(&self.path)?;

        debug!("Executing command: {} -d {}", self.path, interface);
        let status = Command::new(&self.path)
            .args(["-d", interface])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .context(format!("Failed to execute {}", &self.path))?;

        if !status.success() {
            bail!("Command {} exited with {}", &self.path, status)
        }
        Ok(())
    }
}

fn check_permissions(path: &str) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let meta = fs::metadata(path).context(format!("Failed to read {}", path))?;
    let mode = meta.permissions().mode();
    let file_uid = Uid::from_raw(meta.uid());

    if mode & 0o020 != 0 || mode & 0o002 != 0 {
        bail!(
            "File {} is writeable by group or others (mode {:o})",
            path,
            mode,
        );
    }
    if !file_uid.is_root() && file_uid != Uid::current() {
        bail!(
            "File {} is not owned by root nor the current user (uid {}), but by uid {}",
            path,
            Uid::current(),
            file_uid,
        );
    }
    Ok(())
}
