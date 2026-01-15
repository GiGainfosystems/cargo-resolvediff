// Copyright (C) 2026 by GiGa infosystems

//! Walks the `resolve` graph in an [`IndexedMetadata`] to gather dependency kinds & inclusion
//! reasons

use crate::Platform;
use crate::indexed::IndexedMetadata;
use camino::{Utf8Path, Utf8PathBuf};
use cargo_metadata::PackageId;
use color_eyre::Result;
use semver::Version;
use serde::Serialize;
use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet, btree_map},
    fmt,
    path::Path,
};

fn shorten_path_relative_to<'a>(relative: &Utf8Path, path: &'a Utf8Path) -> &'a Utf8Path {
    if path.starts_with(relative) {
        path.strip_prefix(relative).expect("checked above")
    } else {
        path
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
enum AnyCrateIdent {
    Local(Utf8PathBuf),
    CratesIo(String),
}

impl AnyCrateIdent {
    fn from_package(relative: &Utf8Path, package: &cargo_metadata::Package) -> Self {
        if package.source.is_some() {
            AnyCrateIdent::CratesIo(package.name.to_string())
        } else {
            let path = package.manifest_path.parent().expect("ends in /Cargo.toml");
            AnyCrateIdent::Local(shorten_path_relative_to(relative, path).to_owned())
        }
    }

    fn with_version(self, version: &Version) -> SpecificAnyCrateIdent {
        match self {
            AnyCrateIdent::CratesIo(name) => SpecificAnyCrateIdent::CratesIo(SpecificCrateIdent {
                name,
                version: version.clone(),
            }),
            AnyCrateIdent::Local(manifest_path) => SpecificAnyCrateIdent::Local(manifest_path),
        }
    }
}

// A [crates.io] dependency with a specific version
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct SpecificCrateIdent {
    pub name: String,
    pub version: Version,
}

impl fmt::Debug for SpecificCrateIdent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "SpecificCrateIdent({self})")
    }
}

impl fmt::Display for SpecificCrateIdent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "\"{} {}\"", self.name, self.version)
    }
}

/// A [crates.io] dependency or a local dependency
///
/// (At the moment `git` dependencies get resolved as [crates.io] dependencies even if they are
/// not)
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum SpecificAnyCrateIdent {
    Local(Utf8PathBuf),
    CratesIo(SpecificCrateIdent),
}

impl fmt::Display for SpecificAnyCrateIdent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SpecificAnyCrateIdent::Local(local) => write!(f, "{:?}", local),
            SpecificAnyCrateIdent::CratesIo(ident) => write!(f, "{}", ident),
        }
    }
}

/// The kind of a dependency regarding when it is built or run
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DependencyKind {
    /// The crate gets executed at some point at build time
    pub run_at_build: bool,
    /// The crate is only ever built as a `dev-dependency`
    pub only_debug_builds: bool,
}

impl From<cargo_metadata::DependencyKind> for DependencyKind {
    fn from(dependency_kind: cargo_metadata::DependencyKind) -> Self {
        match dependency_kind {
            cargo_metadata::DependencyKind::Normal => DependencyKind::NORMAL,
            cargo_metadata::DependencyKind::Development => DependencyKind::DEVELOPMENT,
            cargo_metadata::DependencyKind::Build => DependencyKind::BUILD,
            kind => panic!("Unsupported dependency kind in `cargo_metadata`: {kind:?}"),
        }
    }
}

impl DependencyKind {
    /// A dependency that is not run at build and included in release builds
    pub const NORMAL: Self = DependencyKind {
        run_at_build: false,
        only_debug_builds: false,
    };

    /// A dependency that is not run at build and only included via `dev-dependencies`
    pub const DEVELOPMENT: Self = DependencyKind {
        run_at_build: false,
        only_debug_builds: true,
    };

    /// A dependency that is run at build time for release builds
    pub const BUILD: Self = DependencyKind {
        run_at_build: true,
        only_debug_builds: false,
    };

    /// Combine dependency kinds between a parent dependency and its edge to a child.
    ///
    /// If either is a build dependency, this sets `run_at_build`, and if either is only included
    /// in `dev-dependencies`, this sets `only_debug_builds`.
    pub const fn then(self, next: DependencyKind) -> Self {
        DependencyKind {
            run_at_build: self.run_at_build || next.run_at_build,
            only_debug_builds: self.only_debug_builds || next.only_debug_builds,
        }
    }

    /// Combine dependency kinds for a crate version coming from different paths.
    ///
    /// If either is a build dependency, this sets `run_at_build`, and `only_debug_builds` is only
    /// set if both are only reachable via `dev-dependencies`.
    pub const fn merged_with(self, other: DependencyKind) -> Self {
        DependencyKind {
            run_at_build: self.run_at_build || other.run_at_build,
            only_debug_builds: self.only_debug_builds && other.only_debug_builds,
        }
    }
}

impl fmt::Debug for DependencyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.run_at_build, self.only_debug_builds) {
            (false, false) => write!(f, "DependencyKind::NORMAL"),
            (false, true) => write!(f, "DependencyKind::DEBUG"),
            (true, false) => write!(f, "DependencyKind::BUILD"),
            (true, true) => write!(f, "DependencyKind::DEBUG.then(DependencyKind::BUILD)"),
        }
    }
}

// NOTE: The intermediate dependencies may be local dependencies due to feature resolution, or path
// dependencies outside of the workspace.
/// The reason for the inclusion of a dependency in its specific form.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IncludedDependencyReason {
    /// The kind of inclusion edge, which is the [`DependencyKind`] of the parent
    pub kind: DependencyKind,
    /// The `Cargo.toml` in the workspace that this originated from
    pub root: Utf8PathBuf,
    /// The dependency in that `Cargo.toml` that then at some point ends up depending on `parent`
    /// (if this is `None`, the dependency in the `Cargo.toml` is `parent`)
    pub intermediate_root_dependency: Option<SpecificAnyCrateIdent>,
    /// The dependency that directly depended on this crate
    pub parent: SpecificAnyCrateIdent,
}

impl fmt::Debug for IncludedDependencyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IncludedDependencyReason({self})")
    }
}

impl fmt::Display for IncludedDependencyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.root != "" {
            write!(f, "{:?}", self.root)?;
        }
        if let Some(ref intermediate) = self.intermediate_root_dependency {
            write!(f, ".{intermediate}")?;
            if self.parent != *intermediate {
                write!(f, "...{}", self.parent)?;
            }
        }
        Ok(())
    }
}

impl Serialize for IncludedDependencyReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_string().serialize(serializer)
    }
}

/// The reasons for a dependencies inclusion mapped to a set of platforms.
///
/// NOTE: This set may be empty if an [`IndexedMetadata`] was included that didn't filter for a
/// platform.
pub type Reasons = BTreeMap<IncludedDependencyReason, BTreeSet<Platform>>;

/// NOTE: Only keeps track of platforms that are explicitly listed in [`IndexedMetadata`]s that
/// were passed, or alternatively the platforms given to [`Resolved::resolve_for`].
pub struct IncludedDependencyVersion {
    pub kind: DependencyKind,
    pub has_build_rs: bool,
    pub is_proc_macro: bool,
    /// The reasons for the inclusion of this crate
    pub reasons: Reasons,
    /// The platforms this crate is included for that were filtered for in an [`IndexedMetadata`]
    pub platforms: BTreeSet<Platform>,
}

/// The set of included packages, mapping from the crate name to a map from versions to the actual
/// metadata
pub type Included = BTreeMap<String, BTreeMap<Version, IncludedDependencyVersion>>;

/// The set of fully resolved information ready for diffing with [`crate::diff::Diff`]
pub struct Resolved {
    /// The [`IndexedMetadata`] this is based on
    pub full_metadata: IndexedMetadata,
    /// The set of packages that are included in the filtered platforms, or all packages if an
    /// unfiltered [`IndexedMetadata`] was included
    pub included: Included,
    /// The set of filtered packages, or
    pub filtered: BTreeSet<SpecificCrateIdent>,
}

impl Resolved {
    /// Resolve everything only for a given platform given its filtered [`IndexedMetadata`] (or the
    /// unfiltered metadata if all platforms should be included)
    fn resolve_platform(metadata: &IndexedMetadata, included: &mut Included) {
        #[derive(Clone)]
        enum TodoFrom<'a> {
            Workspace(&'a Utf8Path),
            Dependency(IncludedDependencyReason),
        }

        struct Todo<'a> {
            kind: DependencyKind,
            incoming_edge: TodoFrom<'a>,
            pkg: &'a PackageId,
        }

        let mut todos = metadata
            .get_workspace_default_members()
            .iter()
            .map(|pkg| {
                let path = shorten_path_relative_to(
                    &metadata.workspace_root,
                    &metadata.packages[pkg].manifest_path,
                );
                Todo {
                    kind: DependencyKind::NORMAL,
                    incoming_edge: TodoFrom::Workspace(path),
                    pkg,
                }
            })
            .collect::<Vec<_>>();

        while let Some(todo) = todos.pop() {
            let package = &metadata.packages[todo.pkg];
            let node = &metadata.resolve[todo.pkg];

            let package_ident = AnyCrateIdent::from_package(&metadata.workspace_root, package);

            let mut has_build_rs = false;
            let mut is_proc_macro = false;
            for target in package.targets.iter().flat_map(|target| &target.kind) {
                use cargo_metadata::TargetKind;

                match target {
                    TargetKind::Bench
                    | TargetKind::Bin
                    | TargetKind::CDyLib
                    | TargetKind::DyLib
                    | TargetKind::Example
                    | TargetKind::Lib
                    | TargetKind::RLib
                    | TargetKind::StaticLib
                    | TargetKind::Test => (),
                    TargetKind::CustomBuild => has_build_rs = true,
                    TargetKind::ProcMacro => is_proc_macro = true,
                    _ => panic!("Unknown target kind"),
                }
            }

            let mut package_kind = todo.kind;
            if is_proc_macro {
                package_kind.run_at_build = true;
            }

            if let AnyCrateIdent::CratesIo(ref name) = package_ident {
                let version = included
                    .entry(name.clone())
                    .or_default()
                    .entry(package.version.clone());
                let inserted_new = matches!(version, btree_map::Entry::Vacant(_));
                let version = version.or_insert_with(|| IncludedDependencyVersion {
                    kind: package_kind,
                    has_build_rs,
                    is_proc_macro,
                    reasons: BTreeMap::new(),
                    platforms: BTreeSet::new(),
                });

                let package_kind = version.kind.merged_with(package_kind);
                let new_kind = package_kind != version.kind;
                version.kind = package_kind;

                // NOTE: A new reason isn't a cause to re-explore, as showing _some_ reasons is likely
                // enough
                match todo.incoming_edge {
                    TodoFrom::Workspace(_) => (),
                    TodoFrom::Dependency(ref reason) => {
                        let entry = version.reasons.entry(reason.clone()).or_default(); // This gets added even if we don't add a platform
                        if let Some(platform) = metadata.platform.clone() {
                            entry.insert(platform);
                        }
                    }
                };

                let new_platform = metadata
                    .platform
                    .clone()
                    .is_some_and(|platform| version.platforms.insert(platform));

                if !(inserted_new || new_kind || new_platform) {
                    continue;
                }
            }

            let dep_parent = package_ident.with_version(&package.version);

            todos.extend(node.deps.iter().filter_map(|dep| {
                let dep_kind = dep
                    .dep_kinds
                    .iter()
                    .filter(|kind| {
                        // Dev dependencies of dependencies are not relevant
                        matches!(todo.incoming_edge, TodoFrom::Workspace(_))
                            || kind.kind != cargo_metadata::DependencyKind::Development
                    })
                    .map(|kind| package_kind.then(kind.kind.into()))
                    .reduce(DependencyKind::merged_with)?;

                let (root, intermediate_root_dependency) = match todo.incoming_edge {
                    TodoFrom::Workspace(root) => (root.to_owned(), None),
                    TodoFrom::Dependency(ref reason) => {
                        let intermediate_root_dependency = reason
                            .intermediate_root_dependency
                            .clone()
                            .unwrap_or_else(|| dep_parent.clone());

                        (reason.root.clone(), Some(intermediate_root_dependency))
                    }
                };

                Some(Todo {
                    kind: dep_kind,
                    incoming_edge: TodoFrom::Dependency(IncludedDependencyReason {
                        kind: package_kind,
                        root,
                        intermediate_root_dependency,
                        parent: dep_parent.clone(),
                    }),
                    pkg: &dep.pkg,
                })
            }));
        }
    }

    /// Resolve everything from a given set of [`IndexedMetadata`]
    pub fn resolve_from_indexed(
        included: impl IntoIterator<Item: Borrow<IndexedMetadata>>,
    ) -> Included {
        let mut out = Included::new();
        for included in included {
            Self::resolve_platform(included.borrow(), &mut out);
        }
        out
    }

    /// Resolve the filtered dependencies from the given [`Included`] data and the set of
    /// unfiltered [`IndexedMetadata`]
    pub fn resolve_filtered_from_indexed(
        included: Included,
        full_metadata: IndexedMetadata,
    ) -> Self {
        assert_eq!(full_metadata.platform, None);

        let mut filtered = BTreeSet::new();

        for pkg in full_metadata.packages.values() {
            if let AnyCrateIdent::CratesIo(name) =
                AnyCrateIdent::from_package(&full_metadata.workspace_root, pkg)
            {
                let was_included = included
                    .get(&name)
                    .is_some_and(|versions| versions.contains_key(&pkg.version));
                if !was_included {
                    filtered.insert(SpecificCrateIdent {
                        name,
                        version: pkg.version.clone(),
                    });
                }
            }
        }

        Resolved {
            full_metadata,
            included,
            filtered,
        }
    }

    /// Resolve everything for a given root manifest for the given set of platforms
    pub fn resolve_from_path(
        root_cargo_toml: &Path,
        specific_platforms: impl IntoIterator<Item = Platform>,
        include_all_platforms: bool,
    ) -> Result<Self> {
        let mut included = itertools::process_results(
            specific_platforms
                .into_iter()
                .map(|platform| IndexedMetadata::gather(root_cargo_toml, Some(platform))),
            |iter| Self::resolve_from_indexed(iter),
        )?;

        let full_metadata = IndexedMetadata::gather(root_cargo_toml, None)?;
        let out = if include_all_platforms {
            Self::resolve_platform(&full_metadata, &mut included);
            Resolved {
                full_metadata,
                included,
                filtered: BTreeSet::new(),
            }
        } else {
            Self::resolve_filtered_from_indexed(included, full_metadata)
        };

        Ok(out)
    }
}
