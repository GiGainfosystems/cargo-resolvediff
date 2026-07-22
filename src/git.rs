// Copyright (C) 2026 by GiGa infosystems

//! Git helpers for the application to add changes & commit them

use crate::cmd::Command;
use anyhow::Result;
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

    fn cmd<const N: usize>(&self, cmd: [&str; N]) -> Command<'_> {
        Command::builder()
            .at_path_maybe(self.path.as_deref())
            .cmd(cmd)
    }

    /// `git add` a given path if it includes changes.
    pub fn add(&mut self, path: &Path) -> Result<()> {
        let changed = !self
            .cmd(["git", "diff"])
            .arg(["-s", "--exit-code"])
            .arg("--")
            .arg(path)
            .is_success()?;
        if changed {
            self.dirty = true;
            self.cmd(["git", "add"]).arg(path).run()?;
        }
        Ok(())
    }

    /// Returns the current commit ID
    pub fn current_commit(&self) -> Result<String> {
        self.cmd(["git", "rev-parse"]).arg("HEAD").stdout()
    }

    /// `git commit` everything that got added, if there were any changes, and return the commit
    /// ID.
    ///
    /// If there were no changes, it returns `Ok(None)`.
    pub fn commit(&mut self, message: &str) -> Result<Option<String>> {
        if !self.dirty {
            return Ok(None);
        }
        self.cmd(["git", "commit"]).arg(("-m", message)).run()?;
        self.dirty = false;
        Ok(Some(self.current_commit()?))
    }

    /// Returns the current branch, if any, or the current commit ID
    pub fn current_branch_or_commit(&self) -> Result<String> {
        let branch = self.cmd(["git", "branch"]).arg("--show-current").stdout()?;
        if !branch.is_empty() {
            Ok(branch)
        } else {
            Ok(self.current_commit()?)
        }
    }

    /// Checks out a given branch or commit ID
    pub fn checkout(&mut self, target: &str) -> Result<()> {
        self.cmd(["git", "checkout"]).arg(target).run()
    }
}
