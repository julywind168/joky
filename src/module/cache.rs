//! Versioned MIR cache. A cache failure is a miss, never executable input.
use super::*;
use bincode::Options;

const MAGIC: &[u8; 8] = b"JKMIR020";
const LIMIT: u64 = 64 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedArtifact {
    key: ModuleCacheKey,
    artifact: ModuleArtifact,
}

fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(LIMIT)
        .reject_trailing_bytes()
}

impl ModuleCache {
    pub(crate) fn load_artifact(
        &self,
        key: &ModuleCacheKey,
    ) -> Result<Option<ModuleArtifact>, String> {
        if !self.enabled {
            return Ok(None);
        }
        let path = self.path(key).with_extension("jmir");
        let bytes = match std::fs::metadata(&path) {
            Ok(info) if info.len() <= LIMIT => std::fs::read(&path).map_err(|e| e.to_string())?,
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if bytes.len() < 16 || &bytes[..8] != MAGIC {
            return Ok(None);
        }
        let checksum = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        if source_fingerprint(&bytes[16..]) != checksum {
            return Ok(None);
        }
        let Ok(cached) = codec().deserialize::<CachedArtifact>(&bytes[16..]) else {
            return Ok(None);
        };
        if cached.key != *key
            || cached.artifact.metadata.format_version != ABI_METADATA_VERSION
            || (key.module_identity != StableId(0)
                && cached.artifact.metadata.stable_id != key.module_identity)
        {
            return Ok(None);
        }
        // Write-side lowering already verified this MIR. Hits trust the checksum
        // and cache key; the linker verifies the merged program again.
        Ok(Some(cached.artifact))
    }

    pub(crate) fn store_artifact(
        &self,
        key: &ModuleCacheKey,
        artifact: &ModuleArtifact,
    ) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        // Serialize the same tuple shape as CachedArtifact without cloning MIR.
        let payload = codec()
            .serialize(&(key, artifact))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(payload.len() + 16);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&source_fingerprint(&payload).to_le_bytes());
        bytes.extend_from_slice(&payload);
        std::fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let path = self.path(key).with_extension("jmir");
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temp = path.with_extension(format!(
            "jmir.{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let result = std::fs::write(&temp, bytes).and_then(|_| std::fs::rename(&temp, &path));
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result.map_err(|e| format!("cannot store module artifact: {e}"))
    }
}

const OBJECT_MAGIC: &[u8; 8] = b"JKAOT001";
const OBJECT_LIMIT: u64 = 128 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedAotObject {
    key: AotObjectCacheKey,
    artifact: CachedAotPayload,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedAotPayload {
    bytes: Vec<u8>,
    machine_entries: Vec<(usize, String)>,
    providers: ProviderMetadataList,
    main_returns_result: bool,
    main_is_suspending: bool,
}

fn object_codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(OBJECT_LIMIT)
        .reject_trailing_bytes()
}

impl ModuleCache {
    pub(crate) fn load_object(
        &self,
        key: &AotObjectCacheKey,
    ) -> Result<Option<AotObjectArtifact>, String> {
        if !self.enabled {
            return Ok(None);
        }
        let path = self.object_path(key);
        let bytes = match std::fs::metadata(&path) {
            Ok(info) if info.len() <= OBJECT_LIMIT => {
                std::fs::read(&path).map_err(|e| e.to_string())?
            }
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if bytes.len() < 16 || &bytes[..8] != OBJECT_MAGIC {
            return Ok(None);
        }
        let checksum = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        if source_fingerprint(&bytes[16..]) != checksum {
            return Ok(None);
        }
        let Ok(cached) = object_codec().deserialize::<CachedAotObject>(&bytes[16..]) else {
            return Ok(None);
        };
        if cached.key != *key {
            return Ok(None);
        }
        Ok(Some(AotObjectArtifact {
            bytes: cached.artifact.bytes,
            machine_entries: cached.artifact.machine_entries,
            providers: cached.artifact.providers,
            main_returns_result: cached.artifact.main_returns_result,
            main_is_suspending: cached.artifact.main_is_suspending,
        }))
    }

    pub(crate) fn store_object(
        &self,
        key: &AotObjectCacheKey,
        artifact: &AotObjectArtifact,
    ) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let payload = object_codec()
            .serialize(&(
                key,
                CachedAotPayload {
                    bytes: artifact.bytes.clone(),
                    machine_entries: artifact.machine_entries.clone(),
                    providers: artifact.providers.clone(),
                    main_returns_result: artifact.main_returns_result,
                    main_is_suspending: artifact.main_is_suspending,
                },
            ))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(payload.len() + 16);
        bytes.extend_from_slice(OBJECT_MAGIC);
        bytes.extend_from_slice(&source_fingerprint(&payload).to_le_bytes());
        bytes.extend_from_slice(&payload);
        std::fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let path = self.object_path(key);
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temp = path.with_extension(format!(
            "jo.{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let result = std::fs::write(&temp, bytes).and_then(|_| std::fs::rename(&temp, &path));
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result.map_err(|e| format!("cannot store AOT object: {e}"))
    }
}
