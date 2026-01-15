// Copyright (C) 2026 by GiGa infosystems

//! Git helpers for the application to add changes & commit them

use crate::cmd::cmd;
use color_eyre::Result;
use std::path::{Path, PathBuf};

/// A `git` repository
pub struct Repository {
    /// The path to the repository
    path: Option<PathBuf>,
    /// If any changes got `git add`ed to the repository
    dirty: bool,
}

impl Repository {
    /// Open an existing [`Repository`] at the given path.
    ///
    /// This does not check if the repository actually exist, methods on this type will simply fail
    /// if it doesn't.
    pub fn new(path: Option<PathBuf>) -> Self {
        Repository { path, dirty: false }
    }

    /// `git add` a given path if it includes changes.
    pub fn add(&mut self, path: &Path) -> Result<()> {
        let changed = !cmd!([git diff] ["-s" "--exit-code" "--" (path)] -> bool in &self.path)?;
        if changed {
            self.dirty = true;
            cmd!([git add] [(path)] in &self.path)?;
        }
        Ok(())
    }

    /// `git commit` everything that got added, if there were any changes, and return the commit
    /// ID.
    ///
    /// If there were no changes, it returns `Ok(None)`.
    pub fn commit(&mut self, message: &str) -> Result<Option<String>> {
        if !self.dirty {
            return Ok(None);
        }
        cmd!([git commit] ["-m" (message)] in &self.path)?;
        self.dirty = false;
        let out = cmd!([git "rev-parse"] [HEAD] -> String in &self.path)?;
        Ok(Some(out))
    }
}
