// Copyright (C) 2026 by GiGa infosystems

//! Run `cargo metadata` & index the result by [`PackageId`]s

use std::{collections::HashMap, path::Path};

use crate::Platform;
use camino::Utf8PathBuf;
use cargo_metadata::{MetadataCommand, Node, Package, PackageId};
use color_eyre::Result;

/// The indexed output of `cargo metadata`
#[derive(Debug)]
pub struct IndexedMetadata {
    /// The platform `cargo metadata` ran for (via `--filter-platform`).
    ///
    /// If it is `None`, this contains all packages for all platforms.
    pub platform: Option<Platform>,
    /// The packages indexed by [`PackageId`]s
    pub packages: HashMap<PackageId, Package>,
    /// The resolution graph indexed by [`PackageId`]s
    pub resolve: HashMap<PackageId, Node>,
    /// The root directory of the workspace (the directory containing the manifest)
    pub workspace_root: Utf8PathBuf,
    /// The list of members in this workspace
    pub workspace_members: Vec<PackageId>,
    /// The default members of this workspace. Contrary to [`cargo_metadata`], this is represented
    /// as an `Option` instead of panicking on access if it was missing.
    pub workspace_default_members: Option<Vec<PackageId>>,
}

impl IndexedMetadata {
    /// Gather & index dependency metadata for the `Cargo.toml` at `path` (for `--manifest-path`),
    /// with the given platform (via `--filter-platform`).
    ///
    /// If `platform` is `None`, this contains all packages for all platforms.
    pub fn gather(path: &Path, platform: Option<Platform>) -> Result<Self> {
        let mut other_options = Vec::new();
        if let Some(ref platform) = platform {
            other_options.extend(["--filter-platform".to_owned(), platform.0.clone()]);
        }
        other_options.push("--locked".to_owned());

        let data = MetadataCommand::new()
            .manifest_path(path)
            .other_options(other_options)
            .exec()?;

        let packages = data
            .packages
            .into_iter()
            .map(|pkg| (pkg.id.clone(), pkg))
            .collect();

        let resolve = data.resolve.map_or_else(HashMap::new, |resolve| {
            resolve
                .nodes
                .into_iter()
                .map(|node| (node.id.clone(), node))
                .collect()
        });

        let workspace_default_members = data
            .workspace_default_members
            .is_available()
            .then(|| (*data.workspace_default_members).to_owned());

        Ok(IndexedMetadata {
            platform,
            packages,
            resolve,
            workspace_root: data.workspace_root,
            workspace_members: data.workspace_members,
            workspace_default_members,
        })
    }

    /// Return the default members, or if they are missing, all workspace members
    pub fn get_workspace_default_members(&self) -> &[PackageId] {
        self.workspace_default_members
            .as_ref()
            .unwrap_or(self.workspace_members.as_ref())
    }
}
