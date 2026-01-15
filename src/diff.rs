// Copyright (C) 2026 by GiGa infosystems

//! Generate a diff between two [`resolve::Resolved`]s, see [`Diff::between`].

use crate::Platform;
use crate::resolve::{
    DependencyKind, IncludedDependencyReason, IncludedDependencyVersion, Reasons, Resolved,
    SpecificCrateIdent,
};
use semver::Version;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Added dependencies on the right
///
/// These only get emitted if no comparison was emitted for this dependency
#[derive(Serialize, Debug)]
pub struct Added<'a> {
    /// The name & version of the this dependency
    pub ident: SpecificCrateIdent,
    pub kind: DependencyKind,
    pub has_build_rs: bool,
    pub is_proc_macro: bool,
    /// The platform this dependency is built (and potentially run at build time) for
    pub platforms: &'a BTreeSet<Platform>,
    /// The reasons for the inclusion of this dependency
    pub reasons: &'a Reasons,
}

/// Dependencies on the right that are different from dependencies with the same name on the left
/// (in version, kind or platform inclusion)
#[derive(Serialize, Debug)]
pub struct Comparison<'a> {
    /// The name & version of this dependency
    pub ident: SpecificCrateIdent,
    pub kind: DependencyKind,
    pub has_build_rs: bool,
    pub is_proc_macro: bool,
    /// The platform this dependency is built (and potentially run at build time) for
    pub platforms: &'a BTreeSet<Platform>,
    pub reasons: &'a Reasons,

    /// The closest version from the left, or [`None`] if the same version existed (in this case
    /// [`Comparison`]s are only emitted if the `kind` or set of platforms changed)
    pub closest_different_old_version: Option<Version>,
    /// The list of all other versions from the left that are different from this version _and_
    /// different from `closest_different_old_version`
    pub all_other_old_versions: Vec<Version>,

    /// The platforms this version was not built for on the left, but is now, with the reasons for
    /// the addition
    pub added_in_platforms: BTreeMap<&'a Platform, Vec<&'a IncludedDependencyReason>>,
    /// The reasons (mapping to platforms) for this dependency to be run at build time
    pub added_in_build: BTreeMap<&'a IncludedDependencyReason, &'a BTreeSet<Platform>>,
    /// The reasons (mapping to platforms) for this dependency to included outside of dev
    /// dependencies
    pub added_in_non_debug: BTreeMap<&'a IncludedDependencyReason, &'a BTreeSet<Platform>>,
}

impl Comparison<'_> {
    fn requires_review(&self) -> bool {
        self.closest_different_old_version.is_some()
            || !self.added_in_platforms.is_empty()
            || !self.added_in_build.is_empty()
            || !self.added_in_non_debug.is_empty()
    }
}

/// Removed dependencies on the right
///
/// These only get emitted if no comparison was emitted for this dependency
#[derive(Serialize, Debug)]
pub struct Removed {
    /// The name & version of the this dependency
    pub ident: SpecificCrateIdent,
    /// The remaining versions of the same name included on the right
    pub remaining_versions: Vec<Version>,
}

/// The differences (for code reviews of dependencies) between two dependency resolutions
#[derive(Serialize, Debug)]
pub struct Diff<'a> {
    pub added: Vec<Added<'a>>,
    pub changed: Vec<Comparison<'a>>,
    pub removed: Vec<Removed>,
    /// Crate versions that are part of the right but not the left, which weren't included in the
    /// platforms the resolution ran for
    pub filtered_added: Vec<SpecificCrateIdent>,
    /// Crate versions that are part of the left but not the right, which weren't included in the
    /// platforms the resolution ran for
    pub filtered_removed: Vec<SpecificCrateIdent>,
}

impl<'a> Diff<'a> {
    fn compare(
        name: &'a str,
        old: &'a BTreeMap<Version, IncludedDependencyVersion>,
        new_version: Version,
        new: &'a IncludedDependencyVersion,
    ) -> Comparison<'a> {
        // NOTE: The assumption is that checking for removals is probably usually easier,
        // so giving out downgrades for reviews is preferred:
        let (closest_old_version, closest_old_info) =
            old.range(&new_version..).next().unwrap_or_else(|| {
                old.last_key_value()
                    .expect("Higher ones were already checked, version set is never empty")
            });

        let closest_different_old_version =
            (*closest_old_version != new_version).then(|| closest_old_version.clone());

        let all_other_old_versions =
            if let Some(ref already_mentioned) = closest_different_old_version {
                old.keys()
                    .filter(|i| *i != already_mentioned)
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };

        let added_in_platforms = new
            .platforms
            .iter()
            .filter(|i| !closest_old_info.platforms.contains(i))
            .map(|platform| {
                let reasons = new
                    .reasons
                    .iter()
                    .filter(|(_, platforms)| platforms.contains(platform))
                    .map(|(reason, _)| reason)
                    .collect::<Vec<_>>();
                (platform, reasons)
            })
            .collect();

        let added_in_build = if new.kind.run_at_build && !closest_old_info.kind.run_at_build {
            new.reasons
                .iter()
                .filter(|(reason, _)| reason.kind.run_at_build)
                .collect()
        } else {
            BTreeMap::new()
        };

        let added_in_non_debug =
            if !new.kind.only_debug_builds && closest_old_info.kind.only_debug_builds {
                new.reasons
                    .iter()
                    .filter(|(reason, _)| !reason.kind.only_debug_builds)
                    .collect()
            } else {
                BTreeMap::new()
            };

        Comparison {
            ident: SpecificCrateIdent {
                name: name.to_owned(),
                version: new_version,
            },
            kind: new.kind,
            has_build_rs: new.has_build_rs,
            is_proc_macro: new.is_proc_macro,
            platforms: &new.platforms,
            reasons: &new.reasons,

            closest_different_old_version,
            all_other_old_versions,

            added_in_platforms,
            added_in_build,
            added_in_non_debug,
        }
    }

    /// Returns the differences between two [`Resolved`]s for code reviews of dependencies
    pub fn between(old: &'a Resolved, new: &'a Resolved) -> Self {
        let added = new
            .included
            .iter()
            .filter(|(name, _)| !old.included.contains_key(*name))
            .flat_map(|(name, versions)| {
                versions
                    .iter()
                    .map(move |(version, item)| (name, version, item))
            })
            .map(|(name, version, info)| Added {
                ident: SpecificCrateIdent {
                    name: name.clone(),
                    version: version.clone(),
                },
                kind: info.kind,
                has_build_rs: info.has_build_rs,
                is_proc_macro: info.is_proc_macro,
                platforms: &info.platforms,
                reasons: &info.reasons,
            })
            .collect();

        let changed = new
            .included
            .iter()
            .filter_map(|(name, new_versions)| {
                old.included
                    .get(name)
                    .map(|old_versions| (name, old_versions, new_versions))
            })
            .flat_map(|(name, old_versions, new_versions)| {
                new_versions.iter().map(move |(new_version, new_info)| {
                    Self::compare(name, old_versions, new_version.clone(), new_info)
                })
            })
            .filter(|comparison| comparison.requires_review())
            .collect();

        let removed = old
            .included
            .iter()
            .filter_map(|(name, versions)| {
                let new_versions = new.included.get(name);
                let has_change = new_versions
                    .is_some_and(|new| new.keys().any(|key| !versions.contains_key(key)));
                if has_change {
                    // NOTE: This isn't a removal because there is an change of some sort for this
                    // package (= a version that wasn't included previously is now included while
                    // the package did exist before for some version)
                    None
                } else {
                    Some((name, versions, new_versions))
                }
            })
            .flat_map(|(name, versions, new_versions)| {
                let is_in_new = move |version: &Version| {
                    new_versions.is_some_and(|new| new.contains_key(version))
                };
                let remaining_versions = versions
                    .keys()
                    .filter(|version| is_in_new(version))
                    .cloned()
                    .collect::<Vec<_>>();
                versions
                    .keys()
                    .filter(move |version| !is_in_new(version))
                    .map(move |version| Removed {
                        ident: SpecificCrateIdent {
                            name: name.clone(),
                            version: version.clone(),
                        },
                        remaining_versions: remaining_versions.clone(),
                    })
            })
            .collect();

        // NOTE: The type has to be specified to `SpecificCrateIdent` here for some reason even
        // though it should be inferrable:
        let in_right_set = |left: &BTreeSet<SpecificCrateIdent>, right: &BTreeSet<_>| {
            right
                .iter()
                .filter(|item| !left.contains(item))
                .cloned()
                .collect()
        };

        let filtered_added = in_right_set(&old.filtered, &new.filtered);
        let filtered_removed = in_right_set(&old.filtered, &new.filtered);

        Diff {
            added,
            changed,
            removed,
            filtered_added,
            filtered_removed,
        }
    }
}
