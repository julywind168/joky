//! Compile-time resource snapshots. Runtime programs never open these files.
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::ModuleLoadError;
use crate::{Diagnostic, Span};

/// Bound resource input per source module, including non-regular file rejection.
pub const MAX_RESOURCE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct EmbeddedResource {
    pub path: PathBuf,
    pub data: Vec<u8>,
}

pub(super) fn snapshot(
    module: &Path,
    source: &str,
    paths: &BTreeMap<String, Span>,
) -> Result<BTreeMap<String, EmbeddedResource>, ModuleLoadError> {
    let mut resources = BTreeMap::new();
    let mut total = 0u64;
    for (relative, span) in paths {
        let fail = |message: String| ModuleLoadError::Diagnostic {
            path: module.to_owned(),
            source: source.to_owned(),
            diagnostic: Diagnostic::parse(message, Some(*span)),
        };
        if relative.is_empty()
            || relative.contains(['\0', '\\', ':'])
            || Path::new(relative).is_absolute()
        {
            return Err(fail(
                "include_bytes requires a nonempty relative path using '/'".into(),
            ));
        }
        let path = module.parent().unwrap_or(Path::new(".")).join(relative);
        let path = path.canonicalize().map_err(|error| {
            fail(format!(
                "cannot read include_bytes resource '{relative}': {error}"
            ))
        })?;
        let read = || -> std::io::Result<Vec<u8>> {
            if !std::fs::metadata(&path)?.is_file() {
                return Err(std::io::Error::other("resource must be a regular file"));
            }
            let file = std::fs::File::open(&path)?;
            if !file.metadata()?.is_file() {
                return Err(std::io::Error::other("resource must be a regular file"));
            }
            let mut data = Vec::new();
            file.take(MAX_RESOURCE_BYTES - total + 1)
                .read_to_end(&mut data)?;
            Ok(data)
        };
        let data = read().map_err(|error| {
            fail(format!(
                "cannot read include_bytes resource '{relative}': {error}"
            ))
        })?;
        total += data.len() as u64;
        if total > MAX_RESOURCE_BYTES {
            return Err(fail(
                "include_bytes resources exceed 16 MiB per module".into(),
            ));
        }
        resources.insert(relative.clone(), EmbeddedResource { path, data });
    }
    Ok(resources)
}

pub(super) fn fingerprints(resources: &BTreeMap<String, EmbeddedResource>) -> Vec<[u8; 32]> {
    resources
        .iter()
        .map(|(path, resource)| {
            let mut hash = Sha256::new();
            hash.update((path.len() as u64).to_le_bytes());
            hash.update(path.as_bytes());
            hash.update(&resource.data);
            hash.finalize().into()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::module::{ModuleGraph, ModuleGraphCache};

    #[test]
    fn include_bytes_snapshot_survives_deletion_and_cached_discovery_reloads_content() {
        let root =
            std::env::temp_dir().join(format!("joky-resource-snapshot-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let entry = root.join("main.jk");
        let resource = root.join("bytes.bin");
        std::fs::write(
            &entry,
            r#"fn main() { let bytes = include_bytes("bytes.bin"); () }"#,
        )
        .unwrap();
        std::fs::write(&resource, [0, 255, 128]).unwrap();
        let mut cache = ModuleGraphCache::default();
        let first = ModuleGraph::load_with_sources_cached(&entry, &root, &[], &mut cache).unwrap();
        let units = first.source_units("runtime", "target", 64, "mir").unwrap();
        assert_eq!(cache.take_stats(), (0, 1));
        std::fs::write(&resource, [128, 255, 0]).unwrap();
        let second = ModuleGraph::load_with_sources_cached(&entry, &root, &[], &mut cache).unwrap();
        let changed = second.source_units("runtime", "target", 64, "mir").unwrap();
        assert_eq!(cache.take_stats(), (1, 0));
        assert_eq!(
            units[0].cache_key.source_hash,
            changed[0].cache_key.source_hash
        );
        assert_ne!(
            units[0].cache_key.fingerprint(),
            changed[0].cache_key.fingerprint()
        );
        std::fs::remove_file(&resource).unwrap();
        units[0].verify().unwrap();
        let (program, _) = crate::syntax::parse_program_resources(
            &units[0].source,
            "main.jk",
            units[0]
                .resources
                .iter()
                .map(|(name, resource)| (name.clone(), resource.data.clone()))
                .collect(),
        )
        .unwrap();
        crate::sema::check_program(&program).unwrap();
        let mut tampered = units[0].clone();
        tampered.resources.get_mut("bytes.bin").unwrap().data[0] = 1;
        assert!(tampered
            .verify()
            .unwrap_err()
            .contains("resource fingerprint"));
        assert!(ModuleGraph::load_with_sources_cached(&entry, &root, &[], &mut cache).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
