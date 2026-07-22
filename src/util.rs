// Copyright (C) 2026 by GiGa infosystems

//! Various utility functions associated with this crate

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Platform;
use crate::cmd::Command;
use anyhow::{Context, Result};

/// Do a `cargo update` for the given root `Cargo.toml` manifest
pub fn update(path: &Path) -> Result<bool> {
    Command::builder()
        .at_path_maybe(Some(path))
        .cmd(["cargo", "update"])
        .is_success()
}

pub fn fetch(path: &Path) -> Result<bool> {
    Command::builder()
        .at_path_maybe(Some(path))
        .cmd(["cargo", "fetch"])
        .is_success()
}

/// Run `cargo check`, optionally in a [`rustwide`] sandbox
pub fn check(
    path: &Path,
    sandbox: Option<&rustwide::Build<'_>>,
    timeout: Option<Duration>,
) -> Result<bool> {
    let no_sandbox = sandbox.is_none();
    Command::builder()
        .in_sandbox_maybe(sandbox)
        .with_timeout_maybe(timeout)
        .at_path_maybe(no_sandbox.then_some(path))
        .cmd(["cargo", "check"])
        .arg("--all-targets")
        .is_success()
}

/// Locate the root `Cargo.toml` from the current working directory
pub fn locate_project() -> Result<PathBuf> {
    let out = Command::cmd(["cargo", "locate-project"])
        .arg("--workspace")
        .arg(("--message-format", "plain"))
        .stdout()?
        .into();
    Ok(out)
}

/// Detect the downloadable toolchain name based on the resolved rustc for the cargo manifest
/// directory
pub fn detect_toolchain(path: &Path) -> Result<String> {
    let out = Command::builder()
        .at_path_maybe(Some(path))
        .cmd(["rustc", "--version"])
        .stdout()?;
    let date = out
        .rsplit_once(' ')
        .and_then(|(_, s)| s.strip_suffix(')'))
        .context("`rustc --version` should have '({commit} {date})` as the end of its output")?;
    let out = if out.contains("nightly") {
        format!("nightly-{date}")
    } else if out.contains("beta") {
        format!("beta-{date}")
    } else {
        out.split(' ')
            .nth(1)
            .context(
                "`rustc --version` should have `rustc {version}` as the beginning of its output",
            )?
            .to_owned()
    };
    Ok(out)
}

/// Return the host platform tuple
pub fn host_platform() -> Result<Platform> {
    let platform_tuple = Command::cmd(["rustc", "--print", "host-tuple"]).stdout()?;
    Ok(Platform(platform_tuple))
}
