//! File tracking for multi-file diagnostics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Unique identifier for a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(usize);

/// Information about a source file.
#[derive(Debug, Clone)]
pub(super) struct FileInfo {
    pub(super) path: PathBuf,
    pub(super) source: String,
}

/// Manages source files for diagnostic reporting.
pub struct Files {
    files: Vec<FileInfo>,
    path_to_id: HashMap<PathBuf, FileId>,
}

impl Files {
    fn normalized_path(path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.strip_prefix(std::env::current_dir().unwrap_or_else(|_| path.to_path_buf()))
                .unwrap_or(path)
                .to_path_buf()
        } else {
            path.to_path_buf()
        }
    }

    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            path_to_id: HashMap::new(),
        }
    }

    /// Add a file to the registry and return its ID.
    pub fn add(&mut self, path: impl AsRef<Path>, source: String) -> FileId {
        let path = Self::normalized_path(path.as_ref());

        // Check if file already exists
        if let Some(&id) = self.path_to_id.get(&path) {
            return id;
        }

        let id = FileId(self.files.len());
        let info = FileInfo {
            path: path.clone(),
            source,
        };

        self.files.push(info);
        self.path_to_id.insert(path, id);
        id
    }

    /// Get file info by ID.
    pub(super) fn get(&self, id: FileId) -> Option<&FileInfo> {
        self.files.get(id.0)
    }

    /// Return the source text registered for a file.
    pub fn source(&self, id: FileId) -> Option<&str> {
        self.get(id).map(|file| file.source.as_str())
    }

    /// Return the displayed path registered for a file.
    pub fn path(&self, id: FileId) -> Option<&Path> {
        self.get(id).map(|file| file.path.as_path())
    }

    /// Return the ID for a path already registered in this collection.
    pub fn id_for_path(&self, path: impl AsRef<Path>) -> Option<FileId> {
        self.path_to_id
            .get(&Self::normalized_path(path.as_ref()))
            .copied()
    }

    /// Iterate over all registered files in stable registration order.
    pub fn iter(&self) -> impl Iterator<Item = (FileId, &Path, &str)> {
        self.files
            .iter()
            .enumerate()
            .map(|(index, file)| (FileId(index), file.path.as_path(), file.source.as_str()))
    }
}

impl Default for Files {
    fn default() -> Self {
        Self::new()
    }
}
