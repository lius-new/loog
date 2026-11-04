//! Authenticated release AOT. Serialized Wasmtime code is native executable code.
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::{digest, signature};
use serde::{Deserialize, Serialize};
use std::{
    hash::{Hash, Hasher},
    io::Read,
    path::Path,
};

pub const PROFILE: &str = "loom-wasmtime-50.0.1-portable-v1";
pub const DOMAIN: &[u8] = b"loom.extension.aot.v1\0";
/// Official compiler authority; deliberately not taken from plugin data or settings.
/// Uses the existing official publication key, with a separate signature domain.
pub const OFFICIAL_KEYS: &[(&str, &str)] = &[(
    "index-2026-1",
    "rB3b9rxiXnaM5RDpE4QYDT5M3aJxWBByg7XePFL5ORo=",
)];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeSource {
    Compiled,
    LocalCache,
    PublishedAot,
}
pub struct LoadedCode {
    pub component: Component,
    pub source: CodeSource,
    pub aot_rejection: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub schema: u32,
    pub id: String,
    pub version: String,
    pub profile: String,
    pub target: String,
    pub compatibility: String,
    pub wasm_sha256: String,
    pub code_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    #[serde(rename = "keyId")]
    key_id: String,
    algorithm: String,
    signature: String,
}

pub fn target() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        ("aarch64", "windows") => "aarch64-pc-windows-msvc",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        _ => "unsupported",
    }
}
/// Kept in sync with the pinned publication compiler. Explicit target disables
/// build-machine CPU tuning; no AVX/CPU-specific artifact is assumed portable.
pub fn portable_engine(target: &str) -> Result<Engine, ExtensionError> {
    let mut config = Config::new();
    config.epoch_interruption(true);
    config.target(target).map_err(|e| failed(&e))?;
    Engine::new(&config).map_err(|e| failed(&e))
}
pub fn compatibility(engine: &Engine) -> String {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    engine.precompile_compatibility_hash().hash(&mut hash);
    format!("{:016x}", hash.finish())
}
pub fn sha256(bytes: &[u8]) -> String {
    digest::digest(&digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn read(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let mut bytes = vec![];
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > maximum {
        return Err("published AOT file exceeds its size limit".into());
    }
    Ok(bytes)
}
impl Runtime {
    /// Application-controlled compiler authorities, useful for an embedding's own
    /// release infrastructure. Never populate this list from an extension package.
    pub fn with_aot_authorities(keys: Vec<(String, String)>) -> Result<Self, ExtensionError> {
        let mut runtime = Self::new()?;
        runtime.aot_keys = keys;
        Ok(runtime)
    }
    fn published_aot(
        &self,
        wasm: &[u8],
        package: &Path,
        id: &str,
        version: &str,
    ) -> Result<Option<Component>, String> {
        let folder = package.join("aot").join(target());
        if !folder.exists() {
            return Ok(None);
        }
        let bytes = read(&folder.join("manifest.json"), 8192)?;
        let signature: Envelope =
            serde_json::from_slice(&read(&folder.join("manifest.sig"), 2048)?)
                .map_err(|e| e.to_string())?;
        if signature.algorithm != "ed25519" {
            return Err("unsupported AOT signature algorithm".into());
        }
        let key = self
            .aot_keys
            .iter()
            .find(|(id, _)| id == &signature.key_id)
            .ok_or("untrusted AOT compiler authority")?;
        let key = STANDARD.decode(&key.1).map_err(|e| e.to_string())?;
        let signature = STANDARD
            .decode(signature.signature)
            .map_err(|e| e.to_string())?;
        signature::UnparsedPublicKey::new(&signature::ED25519, &key)
            .verify(&[DOMAIN, bytes.as_slice()].concat(), &signature)
            .map_err(|_| "published AOT signature does not match")?;
        let metadata: Metadata = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let engine = self
            .aot_engine
            .as_ref()
            .ok_or("unsupported AOT host target")?;
        if metadata.schema != 1
            || metadata.id != id
            || metadata.version != version
            || metadata.profile != PROFILE
            || metadata.target != target()
            || metadata.compatibility != compatibility(engine)
            || metadata.wasm_sha256 != sha256(wasm)
        {
            return Err(
                "published AOT package, Wasmtime profile, target or WASM digest mismatch".into(),
            );
        }
        let code = read(&folder.join("component.cwasm"), 64 * 1024 * 1024)?;
        if sha256(&code) != metadata.code_sha256 {
            return Err("published AOT code digest mismatch".into());
        }
        // SAFETY: an application-pinned compiler authority authenticated these
        // exact native-code bytes, their WASM origin and engine profile. Deserialize
        // owned bytes, not a mutable verified file mapping (avoids a hash/load race).
        unsafe { Component::deserialize(engine, &code) }
            .map(Some)
            .map_err(|e| e.to_string())
    }
    pub fn compile_package(
        &self,
        wasm: &[u8],
        cache: &Path,
        package: &Path,
        id: &str,
        version: &str,
    ) -> Result<LoadedCode, ExtensionError> {
        let rejection = match self.published_aot(wasm, package, id, version) {
            Ok(Some(component)) => {
                return Ok(LoadedCode {
                    component,
                    source: CodeSource::PublishedAot,
                    aot_rejection: None,
                });
            }
            Ok(None) => None,
            Err(error) => Some(error),
        };
        // COMPAT(wasm-only-packages): Existing packages have no authenticated
        // matching AOT, and future engine upgrades may invalidate it. Keep the
        // validated WASM/local-cache path until all supported packages/platforms
        // have compatible signed AOT; no removal version is scheduled.
        let (component, hit) = self.compile_cached(wasm, cache)?;
        Ok(LoadedCode {
            component,
            source: if hit {
                CodeSource::LocalCache
            } else {
                CodeSource::Compiled
            },
            aot_rejection: rejection,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };
    fn fixture() -> (Runtime, Vec<u8>, std::path::PathBuf, Ed25519KeyPair) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let runtime = Runtime::with_aot_authorities(vec![(
            "unit-test".into(),
            STANDARD.encode(pair.public_key().as_ref()),
        )])
        .unwrap();
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let folder = std::env::temp_dir().join(format!(
            "loom-aot-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let wasm = b"(component)".to_vec();
        let dir = folder.join("aot").join(target());
        std::fs::create_dir_all(&dir).unwrap();
        let code = runtime
            .aot_engine
            .as_ref()
            .unwrap()
            .precompile_component(&wasm)
            .unwrap();
        std::fs::write(dir.join("component.cwasm"), &code).unwrap();
        let meta = serde_json::json!({"schema":1,"id":"test.aot","version":"1.0.0","profile":PROFILE,"target":target(),"compatibility":compatibility(runtime.aot_engine.as_ref().unwrap()),"wasm_sha256":sha256(&wasm),"code_sha256":sha256(&code)});
        sign(&folder, &pair, &meta);
        (runtime, wasm, folder, pair)
    }
    fn sign(folder: &Path, pair: &Ed25519KeyPair, meta: &serde_json::Value) {
        let dir = folder.join("aot").join(target());
        let bytes = serde_json::to_vec(meta).unwrap();
        let sig = pair.sign(&[DOMAIN, bytes.as_slice()].concat());
        std::fs::write(dir.join("manifest.json"), bytes).unwrap();
        std::fs::write(dir.join("manifest.sig"),serde_json::to_vec(&serde_json::json!({"keyId":"unit-test","algorithm":"ed25519","signature":STANDARD.encode(sig.as_ref())})).unwrap()).unwrap();
    }
    #[test]
    fn authenticated_first_load_never_creates_a_local_compile_cache() {
        let (runtime, wasm, dir, _) = fixture();
        let cache = dir.join("empty-cache");
        let loaded = runtime
            .compile_package(&wasm, &cache, &dir, "test.aot", "1.0.0")
            .unwrap();
        assert_eq!(loaded.source, CodeSource::PublishedAot);
        assert!(!cache.exists());
        assert!(
            runtime
                .instantiate(&loaded.component, Limits::default())
                .is_err()
        ); // Empty component is valid compiled code, but not Loom's world.
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn altered_native_code_is_rejected_before_deserialization() {
        let (runtime, wasm, dir, _) = fixture();
        std::fs::write(
            dir.join("aot").join(target()).join("component.cwasm"),
            b"not native code",
        )
        .unwrap();
        assert!(
            runtime
                .published_aot(&wasm, &dir, "test.aot", "1.0.0")
                .unwrap_err()
                .contains("digest")
        );
        let loaded = runtime
            .compile_package(&wasm, &dir.join("cache"), &dir, "test.aot", "1.0.0")
            .unwrap();
        assert_eq!(loaded.source, CodeSource::Compiled);
        assert!(loaded.aot_rejection.is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn signatures_identity_wasm_and_engine_are_bound() {
        let (runtime, wasm, dir, pair) = fixture();
        assert!(
            Runtime::new()
                .unwrap()
                .published_aot(&wasm, &dir, "test.aot", "1.0.0")
                .is_err()
        );
        for (id, version, body) in [
            ("other.aot", "1.0.0", wasm.as_slice()),
            ("test.aot", "2.0.0", wasm.as_slice()),
            ("test.aot", "1.0.0", b"(component (type (func)))".as_slice()),
        ] {
            assert!(runtime.published_aot(body, &dir, id, version).is_err());
        }
        let file = dir.join("aot").join(target()).join("manifest.json");
        let original: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        for field in ["profile", "target", "compatibility", "code_sha256"] {
            let mut meta = original.clone();
            meta[field] = "invalid".into();
            sign(&dir, &pair, &meta);
            assert!(
                runtime
                    .published_aot(&wasm, &dir, "test.aot", "1.0.0")
                    .is_err()
            );
        }
        sign(&dir, &pair, &original);
        std::fs::write(&file, b"{}").unwrap();
        assert!(
            runtime
                .published_aot(&wasm, &dir, "test.aot", "1.0.0")
                .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn wasm_only_packages_keep_the_cache_path() {
        let runtime = Runtime::new().unwrap();
        let dir = std::env::temp_dir().join(format!("loom-aot-old-{}", std::process::id()));
        let cache = dir.join("cache");
        let first = runtime
            .compile_package(b"(component)", &cache, &dir, "old.plugin", "1.0.0")
            .unwrap();
        assert_eq!(first.source, CodeSource::Compiled);
        let second = runtime
            .compile_package(b"(component)", &cache, &dir, "old.plugin", "1.0.0")
            .unwrap();
        assert_eq!(second.source, CodeSource::LocalCache);
        drop(first);
        drop(second);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn a_component_cannot_escape_the_runtime_epoch_ticker_via_an_external_engine() {
        let runtime = Runtime::new().unwrap();
        let external = Engine::default();
        let component = Component::new(&external, "(component)").unwrap();
        let error = runtime
            .instantiate(&component, Limits::default())
            .err()
            .unwrap();
        assert!(error.message.contains("runtime-managed epoch"));
    }
}
