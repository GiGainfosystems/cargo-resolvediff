// Copyright (C) 2026 by GiGa infosystems

//! Handle major updates & related tasks

use crate::{
    indexed::IndexedMetadata,
    toml_edit::{MutableTomlFile, TomlPathLookup},
};
use color_eyre::{Result, eyre::eyre};
use crates_io_api::SyncClient;
use itertools::Itertools;
use semver::{Version, VersionReq};
use std::{borrow::Borrow, collections::BTreeMap, iter};
use tinyvec::{ArrayVec, array_vec};

/// Check whether a [`Version`] is considered a major update for a given [`VersionReq`].
///
/// Major updates are defined as:
/// * Versions that don't match the requirement,
/// * which are not pre-releases,
/// * which aren't explicitly matched against using `<` or `<=`,
/// * for which no equal or later version is mentioned in any semver operation
pub fn is_major_update_for(requirement: &VersionReq, version: &Version) -> bool {
    if requirement.matches(version) {
        return false;
    }

    // NOTE: Don't automatically update pre-releases
    if !version.pre.is_empty() {
        return false;
    }

    let stripped_version = Version {
        build: semver::BuildMetadata::EMPTY,
        pre: semver::Prerelease::EMPTY,
        ..*version
    };

    for i in &requirement.comparators {
        let i_version = Version {
            major: i.major,
            minor: i.minor.unwrap_or(version.minor),
            patch: i.patch.unwrap_or(version.patch),
            pre: semver::Prerelease::EMPTY,
            build: semver::BuildMetadata::EMPTY,
        };

        match i.op {
            semver::Op::Less | semver::Op::LessEq => {
                if i_version == stripped_version {
                    // This version was explicitly not matched against
                    return false;
                }
            }
            semver::Op::Exact
            | semver::Op::Greater
            | semver::Op::GreaterEq
            | semver::Op::Tilde
            | semver::Op::Caret => {
                if i_version >= stripped_version {
                    return false;
                }
            }
            semver::Op::Wildcard => unreachable!("Should've matched this version already"),
            op => panic!("Unknown semver operation: {op:?}"),
        }
    }

    true
}

/// Fetch all versions for a crate that have not been yanked.
pub fn fetch_versions_for(
    client: &SyncClient,
    package: &str,
) -> Result<Option<impl Iterator<Item = Version>>> {
    let info = match client.get_crate(package) {
        Ok(info) => info,
        Err(crates_io_api::Error::NotFound(_)) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let versions = info
        .versions
        .into_iter()
        .filter(|version| !version.yanked)
        .map(|version| {
            version
                .num
                .parse::<Version>()
                .expect("Published crate version should be a valid `semver` version")
        });
    Ok(Some(versions))
}

/// Fetch all versions of a crate that are considered major updates for _any_ of the given
/// [`VersionReq`]s and have not been yanked
pub fn fetch_major_updates_for(
    client: &SyncClient,
    package: &str,
    reqs: impl Iterator<Item: Borrow<VersionReq>> + Clone,
) -> Result<Option<impl Iterator<Item = Version>>> {
    let Some(versions) = fetch_versions_for(client, package)? else {
        return Ok(None);
    };
    let versions = versions.filter(move |version| {
        reqs.clone()
            .any(|version_req| is_major_update_for(version_req.borrow(), version))
    });
    Ok(Some(versions))
}

/// The result of [`fetch_latest_major_update_for`]
pub enum LatestVersion {
    CrateNotFound,
    NoMajorUpdates,
    NewestUpdate(Version),
}

/// Fetch the latest versions of a crate that is considered a major update for _any_ of the given
/// [`VersionReq`]s and has not been yanked
pub fn fetch_latest_major_update_for(
    client: &SyncClient,
    package: &str,
    reqs: impl Iterator<Item: Borrow<VersionReq>> + Clone,
) -> Result<LatestVersion> {
    let Some(versions) = fetch_major_updates_for(client, package, reqs)? else {
        return Ok(LatestVersion::CrateNotFound);
    };
    let newest = versions.max();
    Ok(newest.map_or(LatestVersion::NoMajorUpdates, LatestVersion::NewestUpdate))
}

/// A reference to a [crates.io] dependency version, part of [`ManifestDependencySet`]
pub struct DependencyMention {
    manifest_idx: usize,
    /// The TOML path to the version specification
    toml_path: Vec<String>,
    version: VersionReq,
}

impl DependencyMention {
    pub fn toml_path(&self) -> &[String] {
        &self.toml_path
    }

    pub fn version(&self) -> &VersionReq {
        &self.version
    }
}

/// A set of manifests with the associated direct dependencies from [crates.io], with all instances
/// of their version being requested
pub struct ManifestDependencySet {
    pub manifests: ManifestSet,
    /// Maps crate names to [`DependencyMention`]s
    pub dependencies: BTreeMap<String, Vec<DependencyMention>>,
}

impl ManifestDependencySet {
    /// The paths in which dependencies can be listed in a given manifest
    fn dependency_toml_paths(
        manifest: &MutableTomlFile,
    ) -> Result<impl Iterator<Item = ArrayVec<[&str; 3]>>> {
        let targets = manifest
            .document()
            .as_table()
            .get("target")
            .map(|target| {
                target.as_table_like().ok_or_else(|| {
                    eyre!("Invalid target table in {:?} at `target`", manifest.path())
                })
            })
            .transpose()?
            .into_iter()
            .flat_map(|target| target.iter().map(|(key, _)| key));

        let dep_paths = iter::once(None)
            .chain(targets.map(Some))
            .cartesian_product(["dependencies", "build-dependencies", "dev-dependencies"])
            .map(|(target, dep_kind)| {
                target.map_or(
                    array_vec!(_ => dep_kind),
                    |target| array_vec!(_ => "target", target, dep_kind),
                )
            });

        Ok(dep_paths)
    }

    /// Read a version from a given TOML path
    fn read_version(manifest: &MutableTomlFile, path: &[String]) -> Result<VersionReq> {
        let version = manifest
            .path_lookup(path)
            .expect("Version path lookup failed (maybe the `MutableTomlFile` changed?)")
            .as_str()
            .ok_or_else(|| {
                eyre!(
                    "Invalid `version`/immediate value in {path:?} at {:?}",
                    manifest.path()
                )
            })?
            .parse::<VersionReq>()?;
        Ok(version)
    }

    /// Collect all dependencies from a set of manifests
    fn collect_dependencies(
        manifest_idx: usize,
        manifest: &MutableTomlFile,
        direct_dependencies: &mut BTreeMap<String, Vec<DependencyMention>>,
    ) -> Result<()> {
        for dep_path in Self::dependency_toml_paths(manifest)? {
            let Some(dependencies) = manifest.path_lookup(dep_path) else {
                continue;
            };

            let dependencies = dependencies.as_table_like().ok_or_else(|| {
                eyre!(
                    "Invalid dependency table in {:?} at {dep_path}",
                    manifest.path()
                )
            })?;

            for (name, dependency) in dependencies.iter() {
                let (package, version_path_segment) =
                    if let Some(dependency) = dependency.as_table_like() {
                        let package = match dependency.get("package") {
                            None => name,
                            Some(package) => package.as_str().ok_or_else(|| {
                                eyre!(
                                    "Invalid `package` value in {:?} at {dep_path}.{name:?}",
                                    manifest.path()
                                )
                            })?,
                        };

                        if dependency.contains_key("registry")
                            || !dependency.contains_key("version")
                            || dependency.contains_key("git")
                        {
                            continue;
                        }

                        (package, Some("version"))
                    } else {
                        (name, None)
                    };

                let version_path = dep_path
                    .into_iter()
                    .chain(iter::once(name))
                    .chain(version_path_segment)
                    .map(|s| s.to_owned())
                    .collect::<Vec<_>>();

                let version = Self::read_version(manifest, &version_path)?;

                direct_dependencies
                    .entry(package.to_owned())
                    .or_default()
                    .push(DependencyMention {
                        manifest_idx,
                        toml_path: version_path,
                        version,
                    })
            }
        }

        Ok(())
    }

    /// Collect all direct dependencies from all workspace manifests which are part of an
    /// [`IndexedMetadata`]
    pub fn collect(metadata: &IndexedMetadata) -> Result<Self> {
        let manifests = ManifestSet::collect(metadata)?;

        let mut dependencies = BTreeMap::new();
        for (idx, manifest) in manifests.manifests.iter().enumerate() {
            Self::collect_dependencies(idx, manifest, &mut dependencies)?;
        }

        Ok(ManifestDependencySet {
            manifests,
            dependencies,
        })
    }

    /// Commit all changes made to the [`ManifestSet`] (see [`MutableTomlFile::commit`])
    pub fn commit(&mut self) -> Result<()> {
        self.manifests.write_back()?;

        // NOTE: Writing all back before committing allows rolling back if any of the write backs
        // failed
        for manifest in &mut self.manifests.manifests {
            // NOTE: Should now be infallible since it's already been written back
            manifest.commit()?;
        }

        Ok(())
    }

    /// Roll back all changes made to the [`ManifestSet`] (see [`MutableTomlFile::roll_back`]), and
    /// reset the parsed dependency versions to the original values
    pub fn roll_back(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for manifest in &mut self.manifests.manifests {
            if let Err(error) = manifest.roll_back() {
                errors.push(error);
            }
        }

        for mention in self.dependencies.values_mut().flatten() {
            mention.version = Self::read_version(
                &self.manifests.manifests[mention.manifest_idx],
                &mention.toml_path,
            )?;
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(eyre!("Failed to roll back:\n{errors:?}"))
        }
    }
}

/// A set of manifests for a workspace
pub struct ManifestSet {
    manifests: Vec<MutableTomlFile>,
}

impl ManifestSet {
    /// Collect all manifests from an [`IndexedMetadata`]
    pub fn collect(metadata: &IndexedMetadata) -> Result<Self> {
        let workspace_manifest = metadata.workspace_root.join("Cargo.toml");

        let mut member_manifests = metadata
            .packages
            .iter()
            .filter(|(pkg_id, _)| metadata.workspace_members.contains(pkg_id))
            .map(|(_, pkg)| &pkg.manifest_path)
            .collect::<Vec<_>>();

        let isnt_workspace = matches!(*member_manifests, [single] if *single == workspace_manifest);

        if isnt_workspace {
            member_manifests.clear();
        }

        let manifests = iter::once(&workspace_manifest)
            .chain(member_manifests)
            .map(MutableTomlFile::open)
            .collect::<Result<Vec<_>>>()?;

        Ok(ManifestSet { manifests })
    }

    pub fn as_slice(&self) -> &[MutableTomlFile] {
        &self.manifests
    }

    pub fn as_slice_mut(&mut self) -> &mut [MutableTomlFile] {
        &mut self.manifests
    }

    /// Write back all manifests to the underlying files (see [`MutableTomlFile::write_back`])
    pub fn write_back(&mut self) -> Result<()> {
        for manifest in &mut self.manifests {
            manifest.write_back()?;
        }

        Ok(())
    }

    /// Return a reference to  the manifest file associated with a given mention of a dependency
    /// version
    pub fn manifest_for(&self, mention: &DependencyMention) -> &MutableTomlFile {
        &self.manifests[mention.manifest_idx]
    }

    /// Return a mutable reference to the manifest file associated with a given mention of a
    /// dependency version
    pub fn manifest_mut_for(&mut self, mention: &DependencyMention) -> &mut MutableTomlFile {
        &mut self.manifests[mention.manifest_idx]
    }

    /// Write back the changes made to the manifest file associated with a given mention of a
    /// dependency version
    pub fn write_back_for(&mut self, mention: &DependencyMention) -> Result<()> {
        self.manifest_mut_for(mention).write_back()?;
        Ok(())
    }

    /// Write back the changes made to all manifest file associated with any of the given mentions
    /// dependency versions
    pub fn write_back_for_all(&mut self, mentions: &[DependencyMention]) -> Result<()> {
        for mention in mentions {
            self.write_back_for(mention)?;
        }
        Ok(())
    }

    /// Change a dependency version in memory only (requires calling a `write_back` or `commit`
    /// method to actually change the underlying file)
    pub fn write_version_to_memory(
        &mut self,
        mention: &mut DependencyMention,
        version: VersionReq,
    ) {
        let Some(toml_edit::Value::String(toml_version)) = self
            .manifest_mut_for(mention)
            .path_lookup_mut(&mention.toml_path)
            .and_then(toml_edit::Item::as_value_mut)
        else {
            panic!("Version path lookup failed (maybe the `MutableTomlFile` changed?)");
        };
        let decor = toml_version.decor().clone();

        let as_string = match *version.comparators {
            [ref single] if single.op == semver::Op::Caret => {
                let mut out = version.to_string();
                if out.starts_with('^') {
                    out.remove(0); // Remove the caret
                }
                out
            }
            _ => version.to_string(),
        };

        *toml_version = toml_edit::Formatted::new(as_string);
        *toml_version.decor_mut() = decor;

        mention.version = version;
    }

    /// Change a dependency version in memory only (requires calling a `write_back` or `commit`
    /// method to actually change the underlying file) for multiple mentions
    pub fn write_versions_to_memory(
        &mut self,
        mentions: &mut [DependencyMention],
        version: &VersionReq,
    ) {
        for mention in mentions {
            self.write_version_to_memory(mention, version.clone());
        }
    }

    /// Change a dependency version
    pub fn write_version_to_file(
        &mut self,
        mention: &mut DependencyMention,
        version: VersionReq,
    ) -> Result<()> {
        self.write_version_to_memory(mention, version);
        self.write_back_for(mention)?;
        Ok(())
    }

    /// Change a dependency version for multiple mentions
    pub fn write_versions_to_file(
        &mut self,
        mentions: &mut [DependencyMention],
        version: &VersionReq,
    ) -> Result<()> {
        self.write_versions_to_memory(mentions, version);
        self.write_back_for_all(mentions)?;
        Ok(())
    }

    /// Change a dependency version in memory if it is considered a major update
    pub fn update_version_in_memory(&mut self, mention: &mut DependencyMention, version: &Version) {
        if is_major_update_for(&mention.version, version) {
            self.write_version_to_memory(
                mention,
                VersionReq {
                    comparators: vec![semver::Comparator {
                        op: semver::Op::Caret,
                        major: version.major,
                        minor: Some(version.minor),
                        patch: Some(version.patch),
                        pre: version.pre.clone(),
                    }],
                },
            );
        }
    }

    /// Change dependency versions in memory for each mention for which it is considered a major
    /// update
    pub fn update_versions_in_memory(
        &mut self,
        mentions: &mut [DependencyMention],
        version: &Version,
    ) {
        for mention in mentions {
            self.update_version_in_memory(mention, version);
        }
    }

    /// Change a dependency version if it is considered a major update
    pub fn update_version_in_file(
        &mut self,
        mention: &mut DependencyMention,
        version: &Version,
    ) -> Result<()> {
        self.update_version_in_memory(mention, version);
        self.write_back_for(mention)?;
        Ok(())
    }

    /// Change dependency versions for each mention for which it is considered a major update
    pub fn update_versions_in_file(
        &mut self,
        mentions: &mut [DependencyMention],
        version: &Version,
    ) -> Result<()> {
        self.update_versions_in_memory(mentions, version);
        self.write_back_for_all(mentions)?;
        Ok(())
    }
}
