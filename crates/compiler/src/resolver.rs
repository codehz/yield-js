//! Synchronous source resolution; host imports never enter this layer.

use std::borrow::Cow;
use std::collections::BTreeMap;

mod manifest;
mod node;

pub use node::NodeResolver;

use crate::ModuleId;

/// Resolves source specifiers to stable, nonempty canonical module IDs.
pub trait ModuleResolver {
    /// `None` identifies an explicit compilation root.
    fn resolve(&self, referrer: Option<&ModuleId>, specifier: &str) -> Result<ModuleId, String>;
    fn load(&self, id: &ModuleId) -> Result<Cow<'_, str>, String>;
}

impl<R: ModuleResolver + ?Sized> ModuleResolver for &R {
    fn resolve(&self, referrer: Option<&ModuleId>, specifier: &str) -> Result<ModuleId, String> {
        (**self).resolve(referrer, specifier)
    }

    fn load(&self, id: &ModuleId) -> Result<Cow<'_, str>, String> {
        (**self).load(id)
    }
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
    fn resolve(&self, referrer: Option<&ModuleId>, specifier: &str) -> Result<ModuleId, String> {
        normalize(referrer.map(String::as_str), specifier)
    }

    fn load(&self, id: &ModuleId) -> Result<Cow<'_, str>, String> {
        self.files
            .get(id)
            .copied()
            .map(Cow::Borrowed)
            .ok_or_else(|| format!("source file not found: {id:?}"))
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
