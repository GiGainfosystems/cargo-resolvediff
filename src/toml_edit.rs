// Copyright (C) 2026 by GiGa infosystems

//! Utilities for editing `Cargo.toml` manifests

use color_eyre::Result;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item};

/// A mutable TOML file with capabilities to:
/// * Write it back to the filesystem
/// * Roll back to the previously committed version
/// * Commit to the current version
pub struct MutableTomlFile {
    dirty: bool,
    path: PathBuf,
    previous_contents: String,
    document: DocumentMut,
}

impl MutableTomlFile {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let contents = fs::read_to_string(&path)?;
        let document = contents.parse::<DocumentMut>()?;
        Ok(MutableTomlFile {
            dirty: false,
            path,
            previous_contents: contents,
            document,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn document(&self) -> &toml_edit::DocumentMut {
        &self.document
    }

    pub fn document_mut(&mut self) -> &mut toml_edit::DocumentMut {
        self.dirty = true;
        &mut self.document
    }

    fn write_back_inner(&self, data: &str) -> Result<()> {
        let tmp_path = self.path.with_file_name(".Cargo.toml.update");
        fs::write(&tmp_path, data)?;
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    /// Write the TOML file back to the underlying file
    pub fn write_back(&mut self) -> Result<()> {
        if self.dirty {
            self.write_back_inner(&self.document.to_string())?;
            self.dirty = false;
        }

        Ok(())
    }

    /// Roll all changes back to the last commit point (or initial opening of this file)
    pub fn roll_back(&mut self) -> Result<()> {
        self.document = self.previous_contents.parse()?;
        self.write_back_inner(&self.previous_contents)?;
        self.dirty = false;
        Ok(())
    }

    /// Commit to the current version. This cannot error out if it has been written back already.
    pub fn commit(&mut self) -> Result<()> {
        self.write_back()?;
        self.previous_contents = self.document.to_string();
        Ok(())
    }
}

/// Utility to follow paths of string keys in a TOML file.
///
/// This is used to access stored version requirements.
pub trait TomlPathLookup {
    fn path_lookup(&self, path: impl IntoIterator<Item: AsRef<str>>) -> Option<&Item>;
    fn path_lookup_mut(&mut self, path: impl IntoIterator<Item: AsRef<str>>) -> Option<&mut Item>;
}

impl TomlPathLookup for toml_edit::Item {
    fn path_lookup(&self, path: impl IntoIterator<Item: AsRef<str>>) -> Option<&Item> {
        let mut item = self;
        for i in path {
            item = item.get(i.as_ref())?;
        }

        Some(item)
    }

    fn path_lookup_mut(&mut self, path: impl IntoIterator<Item: AsRef<str>>) -> Option<&mut Item> {
        let mut item = self;
        for i in path {
            item = item.get_mut(i.as_ref())?;
        }

        Some(item)
    }
}

impl TomlPathLookup for MutableTomlFile {
    fn path_lookup(&self, path: impl IntoIterator<Item: AsRef<str>>) -> Option<&Item> {
        self.document().as_item().path_lookup(path)
    }

    fn path_lookup_mut(&mut self, path: impl IntoIterator<Item: AsRef<str>>) -> Option<&mut Item> {
        self.document_mut().as_item_mut().path_lookup_mut(path)
    }
}
