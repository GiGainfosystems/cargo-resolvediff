// Copyright (C) 2026 by GiGa infosystems

//! `cargo-resolvediff` is an application that allows to build a diff between the resolved
//! dependency graph between different versions, including automatic updates.
//!
//! The order of operations is:
//! * Gather & index metadata with [`indexed::IndexedMetadata`]
//! * Optionally do major updates with [`major_updates::ManifestDependencySet`]
//! * Resolve the crate graph into a format that stores reasons for inclusion & kinds of
//!   dependencies with [`resolve::Resolved`]
//! * Get a diff between different [`resolve::Resolved`]s with [`diff::Diff`]
//!
//! Currently, only [crates.io] & path dependencies are correctly handled, git dependencies get
//! interpreted as [crates.io] dependencies for diffing purposes.
//!
//! This is fine as long as `git` dependencies aren't automatically updated, or `git` changes
//! point to a branch or are manually updated by someone else.

use serde::Serialize;

/// A platform tuple (such as `x86_64-unknown-linux-gnu`)
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize)]
#[serde(transparent)]
pub struct Platform(pub String);

mod cmd;

pub mod diff;
pub mod git;
pub mod indexed;
pub mod major_updates;
pub mod resolve;
pub mod toml_edit;
pub mod util;
