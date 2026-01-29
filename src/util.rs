// Copyright (C) 2026 by GiGa infosystems

//! Various utility functions associated with this crate

use crate::Platform;
use crate::cmd::cmd;
use color_eyre::Result;
use std::path::{Path, PathBuf};

/// Do a `cargo update` for the given root `Cargo.toml` manifest, optionally running `cargo check`
/// and returning if it succeeded
pub fn update(path: &Path, check: bool) -> Result<bool> {
    if !cmd!([cargo update] ["--manifest-path" (path)] -> bool)? {
        return Ok(false);
    }

    if check && !cmd!([cargo check] ["--manifest-path" (path) "--all-targets"] -> bool)? {
        return Ok(false);
    }

    Ok(true)
}

/// Locate the root `Cargo.toml` from the current working directory
pub fn locate_project() -> Result<PathBuf> {
    let out =
        cmd!([cargo "locate-project"] ["--workspace" "--message-format" plain] -> String)?.into();
    Ok(out)
}

/// Return the host platform tuple
pub fn host_platform() -> Result<Platform> {
    let platform_tuple = cmd!([rustc "--print" "host-tuple"] -> String)?;
    Ok(Platform(platform_tuple))
}
