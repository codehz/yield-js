//! Synchronous source resolution; host imports never enter this layer.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Resolves source specifiers to stable, nonempty canonical module IDs.
pub trait ModuleResolver {
    /// `None` identifies an explicit compilation root.
    fn resolve(&self, referrer: Option<&str>, specifier: &str) -> Result<String, String>;
    fn load(&self, id: &str) -> Result<Cow<'_, str>, String>;
}

pub struct MemoryResolver<'a> {
    files: BTreeMap<String, &'a str>,
}

impl<'a> MemoryResolver<'a> {
    pub fn new(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self, String> {
        let mut map = BTreeMap::new();
        for (name, source) in files {
            let id = normalize(None, name)?;
            if map.insert(id.clone(), source).is_some() {
                return Err(format!("duplicate source ID {id:?}"));
            }
        }
        Ok(Self { files: map })
    }
}

impl ModuleResolver for MemoryResolver<'_> {
    fn resolve(&self, referrer: Option<&str>, specifier: &str) -> Result<String, String> {
        normalize(referrer, specifier)
    }

    fn load(&self, id: &str) -> Result<Cow<'_, str>, String> {
        self.files
            .get(id)
            .copied()
            .map(Cow::Borrowed)
            .ok_or_else(|| format!("source file not found: {id:?}"))
    }
}

pub struct FileResolver {
    root: PathBuf,
}

impl FileResolver {
    pub fn new(root: &Path) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|err| err.to_string())?;
        if !root.is_dir() {
            return Err(format!("source root is not a directory: {root:?}"));
        }
        Ok(Self { root })
    }

    fn file(&self, id: &str) -> Result<PathBuf, String> {
        let path = self
            .root
            .join(id)
            .canonicalize()
            .map_err(|err| err.to_string())?;
        if !path.starts_with(&self.root) {
            return Err(format!("source file is outside the source root: {id:?}"));
        }
        if !path.is_file() {
            return Err(format!("source path is not a file: {id:?}"));
        }
        Ok(path)
    }
}

impl ModuleResolver for FileResolver {
    fn resolve(&self, referrer: Option<&str>, specifier: &str) -> Result<String, String> {
        let path = self.file(&normalize(referrer, specifier)?)?;
        let relative = path.strip_prefix(&self.root).expect("checked source root");
        let mut id = String::new();
        for part in relative.components() {
            let part = part.as_os_str().to_str().ok_or("source ID is not UTF-8")?;
            if part.contains('\\') {
                return Err("backslash in canonical source ID".into());
            }
            if !id.is_empty() {
                id.push('/');
            }
            id.push_str(part);
        }
        Ok(id)
    }

    fn load(&self, id: &str) -> Result<Cow<'_, str>, String> {
        std::fs::read_to_string(self.file(&normalize(None, id)?)?)
            .map(Cow::Owned)
            .map_err(|err| err.to_string())
    }
}

fn normalize(referrer: Option<&str>, specifier: &str) -> Result<String, String> {
    if specifier.is_empty()
        || specifier.starts_with('/')
        || specifier.contains('\\')
        || matches!(specifier.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
    {
        return Err(format!("invalid source path: {specifier:?}"));
    }
    let directory = if specifier.starts_with("./") || specifier.starts_with("../") {
        referrer.and_then(|id| id.rsplit_once('/').map(|(dir, _)| dir))
    } else {
        None
    };
    let mut parts: Vec<_> = directory
        .into_iter()
        .flat_map(|dir| dir.split('/'))
        .collect();
    for part in specifier.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("source path escapes the source root")?;
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err("empty normalized source ID".into());
    }
    Ok(parts.join("/"))
}
