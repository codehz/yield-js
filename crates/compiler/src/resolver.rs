//! Synchronous source resolution; host imports never enter this layer.

use std::borrow::Cow;
use std::collections::BTreeMap;

mod manifest;
mod node;

pub use node::NodeResolver;

use crate::CompileError;
use crate::ModuleId;

/// A canonical module identity together with the framework mark of the module
/// that defines it.
///
/// The mark only decides whether the module contributes Error stack sites; it
/// never affects resolution, linking or runtime semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedModule {
    /// Stable, nonempty canonical module ID; the key accepted by [`ModuleResolver::load`].
    pub id: ModuleId,
    /// Whether the defining module is framework code, whose frames stay hidden.
    pub is_framework: bool,
}

/// Resolves source specifiers to stable, nonempty canonical module IDs.
pub trait ModuleResolver {
    /// `None` identifies an explicit compilation root.
    fn resolve(
        &self,
        referrer: Option<&ModuleId>,
        specifier: &str,
    ) -> Result<ResolvedModule, String>;
    fn load(&self, id: &ModuleId) -> Result<Cow<'_, str>, String>;

    /// Load source for the compiler, optionally returning a diagnostic in the loaded source.
    fn load_for_compile(
        &self,
        id: &ModuleId,
        locate: &dyn Fn(String) -> CompileError,
    ) -> Result<Cow<'_, str>, CompileError> {
        self.load(id).map_err(locate)
    }

    /// The original source ID if this module's positions are in generated code.
    fn generated_source_origin(&self, _id: &str) -> Option<ModuleId> {
        None
    }
}

impl<R: ModuleResolver + ?Sized> ModuleResolver for &R {
    fn resolve(
        &self,
        referrer: Option<&ModuleId>,
        specifier: &str,
    ) -> Result<ResolvedModule, String> {
        (**self).resolve(referrer, specifier)
    }

    fn load(&self, id: &ModuleId) -> Result<Cow<'_, str>, String> {
        (**self).load(id)
    }

    fn load_for_compile(
        &self,
        id: &ModuleId,
        locate: &dyn Fn(String) -> CompileError,
    ) -> Result<Cow<'_, str>, CompileError> {
        (**self).load_for_compile(id, locate)
    }

    fn generated_source_origin(&self, id: &str) -> Option<ModuleId> {
        (**self).generated_source_origin(id)
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
    fn resolve(
        &self,
        referrer: Option<&ModuleId>,
        specifier: &str,
    ) -> Result<ResolvedModule, String> {
        let id = normalize(referrer.map(String::as_str), specifier)?;
        // In-memory sources are authored project code, never framework code.
        Ok(ResolvedModule {
            id,
            is_framework: false,
        })
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
