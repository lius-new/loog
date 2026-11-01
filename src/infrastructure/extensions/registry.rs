//! Fetching, verifying and caching the extension registry index.
//!
//! `index.json` and its detached signature `index.json.sig` are published as assets
//! of the `registry` release of the extensions repository. The signature covers the
//! exact bytes of the index and names the key that made it; Loom carries the public
//! keys it trusts, each with a key id, so a key can be replaced by a new release of
//! Loom without breaking older ones. The index is verified before it is parsed, and
//! again each time it is read back from the cache.
//!
//! An index with a lower `sequence` than one already seen is refused (a replay of
//! a superseded file). An index past its `expires` time is still read, but marked
//! expired: Loom then offers nothing new, and what is installed keeps working.
use crate::features::extensions::registry::{Artifact, Index, check_sequence};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Where the index and its signature are published.
pub const BASE: &str = "https://github.com/lius-new/loom-extensions/releases/download/registry";

/// The index public keys this build trusts: `(key id, raw Ed25519 key in base64)`.
/// `loom-ext keygen` in the extensions repository makes a pair; the private key is
/// that repository's `LOOM_INDEX_KEY` secret. A key being retired stays listed until
/// no published index is signed with it.
pub const KEYS: &[(&str, &str)] = &[(
    "index-2026-1",
    "rB3b9rxiXnaM5RDpE4QYDT5M3aJxWBByg7XePFL5ORo=",
)];

const MAX_INDEX: u64 = 16 * 1024 * 1024;
const MAX_SIGNATURE: u64 = 8 * 1024;
const INDEX_FILE: &str = "index.json";
const SIGNATURE_FILE: &str = "index.json.sig";
const META_FILE: &str = "index.meta.json";
const SNAPSHOT_FILE: &str = "index.snapshot.json";
const SNAPSHOT_SIGNATURE_FILE: &str = "index.snapshot.json.sig";
const LOCK_FILE: &str = "extensions.lock";

/// What the next request tells the server about the copy we have.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conditions {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

pub enum Fetched {
    /// The server says our copy is current.
    NotModified,
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// Gets `url`, sending `Conditions`, reading at most `limit` bytes.
pub type Transport<'a> = dyn Fn(&str, &Conditions, u64) -> Result<Fetched, String> + 'a;

/// Where the answer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Fetched just now (or confirmed current by the server).
    Live,
    /// Read from the cache because the server could not be reached or its answer was refused.
    Cached,
    /// The index this copy of Loom shipped with, because nothing newer was available.
    Snapshot,
}

#[derive(Debug)]
pub struct Loaded {
    pub index: Index,
    /// Past its `expires` time: nothing new is offered from it.
    pub expired: bool,
    pub origin: Origin,
    /// Why the live index was not used, when `origin` is `Cached`.
    pub problem: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Meta {
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
    /// The highest `sequence` ever accepted here, kept even if the cache is replaced.
    #[serde(default)]
    highest_sequence: u64,
    /// When the server last confirmed the cache, in seconds since 1970.
    #[serde(default)]
    checked: i64,
}

/// Check `signature_file` against `index` and read the index. Returns it with the
/// id of the key that signed it.
pub fn verify(index: &[u8], signature_file: &[u8], keys: &[(&str, &str)]) -> Result<Index, String> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(rename = "keyId")]
        key_id: String,
        algorithm: String,
        signature: String,
    }
    let envelope: Envelope = serde_json::from_slice(signature_file)
        .map_err(|_| "the index signature file is not valid".to_owned())?;
    if envelope.algorithm != "ed25519" {
        return Err(format!(
            "the index signature uses an unsupported algorithm {:?}",
            envelope.algorithm
        ));
    }
    let key = keys
        .iter()
        .find(|(id, _)| *id == envelope.key_id)
        .ok_or_else(|| {
            format!(
                "the index is signed with an unknown key {:?}",
                envelope.key_id
            )
        })?
        .1;
    let key = BASE64
        .decode(key)
        .map_err(|_| "a built-in index key is not base64".to_owned())?;
    let signature = BASE64
        .decode(&envelope.signature)
        .map_err(|_| "the index signature is not base64".to_owned())?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(index, &signature)
        .map_err(|_| "the index signature does not match the index".to_owned())?;
    let parsed = Index::parse(index)?;
    // The signed file names its key too; the two must agree.
    if parsed.key_id != envelope.key_id {
        return Err("the index names a different key than the one that signed it".into());
    }
    Ok(parsed)
}

fn read_limited(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

fn read_meta(dir: &Path) -> Meta {
    fs::read_to_string(dir.join(META_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let fail = |error: std::io::Error| format!("{name}: {error}");
    fs::create_dir_all(dir).map_err(fail)?;
    let temporary = dir.join(format!(
        ".{name}-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&temporary, bytes).map_err(fail)?;
    fs::rename(&temporary, dir.join(name)).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        fail(error)
    })
}

/// The cached index, verified again. `None` when nothing is cached or the copy no
/// longer verifies (it is then as good as absent).
pub fn load_cached(dir: &Path, keys: &[(&str, &str)], now: i64) -> Option<Loaded> {
    let index = read_limited(&dir.join(INDEX_FILE), MAX_INDEX)?;
    let signature = read_limited(&dir.join(SIGNATURE_FILE), MAX_SIGNATURE)?;
    let index = verify(&index, &signature, keys).ok()?;
    let expired = index.is_expired(now);
    Some(Loaded {
        index,
        expired,
        origin: Origin::Cached,
        problem: None,
    })
}

/// Ask the server for a newer index, and keep it when it verifies and is not a replay.
fn refresh(
    dir: &Path,
    base: &str,
    keys: &[(&str, &str)],
    now: i64,
    floor: u64,
    transport: &Transport,
) -> Result<Loaded, String> {
    let cached = load_cached(dir, keys, now);
    let mut meta = read_meta(dir);
    // A condition only makes sense for a copy we still have and trust.
    let conditions = match cached {
        Some(_) => Conditions {
            etag: meta.etag.clone(),
            last_modified: meta.last_modified.clone(),
        },
        None => Conditions::default(),
    };
    let none = Conditions::default();
    match transport(&format!("{base}/{INDEX_FILE}"), &conditions, MAX_INDEX)? {
        Fetched::NotModified => {
            let mut cached =
                cached.ok_or("the server reported no change, but nothing is cached")?;
            // A cache older than what shipped with Loom is not "current".
            check_sequence(floor, &cached.index)?;
            meta.checked = now;
            let _ = write_atomic(
                dir,
                META_FILE,
                serde_json::to_string(&meta).unwrap().as_bytes(),
            );
            cached.origin = Origin::Live;
            Ok(cached)
        }
        Fetched::Body {
            bytes,
            etag,
            last_modified,
        } => {
            let signature =
                match transport(&format!("{base}/{SIGNATURE_FILE}"), &none, MAX_SIGNATURE)? {
                    Fetched::Body { bytes, .. } => bytes,
                    Fetched::NotModified => return Err("the signature came back empty".into()),
                };
            let index = verify(&bytes, &signature, keys)?;
            let seen = meta
                .highest_sequence
                .max(floor)
                .max(cached.as_ref().map_or(0, |cached| cached.index.sequence));
            check_sequence(seen, &index)?;
            write_atomic(dir, SIGNATURE_FILE, &signature)?;
            write_atomic(dir, INDEX_FILE, &bytes)?;
            meta = Meta {
                etag,
                last_modified,
                highest_sequence: index.sequence,
                checked: now,
            };
            write_atomic(
                dir,
                META_FILE,
                serde_json::to_string(&meta).unwrap().as_bytes(),
            )?;
            let expired = index.is_expired(now);
            Ok(Loaded {
                index,
                expired,
                origin: Origin::Live,
                problem: None,
            })
        }
    }
}

/// The index this copy of Loom shipped with: `index.snapshot.json` and its signature
/// in `dir`, checked like any other index. `None` when there is none, or it does not verify.
pub fn load_snapshot(dir: &Path, keys: &[(&str, &str)], now: i64) -> Option<Loaded> {
    let index = read_limited(&dir.join(SNAPSHOT_FILE), MAX_INDEX)?;
    let signature = read_limited(&dir.join(SNAPSHOT_SIGNATURE_FILE), MAX_SIGNATURE)?;
    let index = verify(&index, &signature, keys).ok()?;
    let expired = index.is_expired(now);
    Some(Loaded {
        index,
        expired,
        origin: Origin::Snapshot,
        problem: None,
    })
}

/// The index to use: a fresh one when the server gives a good one, else the cached
/// copy or the shipped snapshot, whichever has the higher sequence (with the reason
/// the server's was not used), else an error.
///
/// The snapshot is signed with the same keys and shipped with the release, so its
/// sequence is also a floor: a live index older than it is a replay.
pub fn current(
    dir: &Path,
    snapshot: Option<&Path>,
    base: &str,
    keys: &[(&str, &str)],
    now: i64,
    transport: &Transport,
) -> Result<Loaded, String> {
    let shipped = snapshot.and_then(|dir| load_snapshot(dir, keys, now));
    let floor = shipped.as_ref().map_or(0, |shipped| shipped.index.sequence);
    match refresh(dir, base, keys, now, floor, transport) {
        Ok(loaded) => Ok(loaded),
        Err(problem) => {
            let best = [load_cached(dir, keys, now), shipped]
                .into_iter()
                .flatten()
                .max_by_key(|loaded| loaded.index.sequence);
            match best {
                Some(mut best) => {
                    best.problem = Some(problem);
                    Ok(best)
                }
                None => Err(problem),
            }
        }
    }
}

/// The extensions this copy of Loom ships, as `loom-ext lock` wrote them.
#[derive(Debug, Deserialize)]
pub struct Lock {
    pub schema: u32,
    pub index: LockedIndex,
    pub extensions: Vec<LockedExtension>,
}

/// Which index the lock was made against.
#[derive(Debug, Deserialize)]
pub struct LockedIndex {
    pub sequence: u64,
    #[serde(rename = "keyId")]
    pub key_id: String,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
pub struct LockedExtension {
    pub id: String,
    pub version: String,
    pub artifacts: Vec<Artifact>,
}

/// Read `extensions.lock` in `dir` and check it against the snapshot beside it: the
/// snapshot must verify, be the very bytes the lock hashes, and list each locked
/// version with the packages the lock names.
pub fn load_lock(dir: &Path, keys: &[(&str, &str)]) -> Result<Lock, String> {
    let missing = |name: &str| format!("{} is missing or too large", dir.join(name).display());
    let lock_bytes =
        read_limited(&dir.join(LOCK_FILE), 1024 * 1024).ok_or_else(|| missing(LOCK_FILE))?;
    let lock: Lock = serde_json::from_slice(&lock_bytes)
        .map_err(|error| format!("{LOCK_FILE} is not valid: {error}"))?;
    if lock.schema != 1 {
        return Err(format!("{LOCK_FILE}: unsupported schema {}", lock.schema));
    }
    let snapshot =
        read_limited(&dir.join(SNAPSHOT_FILE), MAX_INDEX).ok_or_else(|| missing(SNAPSHOT_FILE))?;
    let signature = read_limited(&dir.join(SNAPSHOT_SIGNATURE_FILE), MAX_SIGNATURE)
        .ok_or_else(|| missing(SNAPSHOT_SIGNATURE_FILE))?;
    let index = verify(&snapshot, &signature, keys)?;
    if super::package::sha256(&snapshot) != lock.index.sha256
        || index.sequence != lock.index.sequence
        || index.key_id != lock.index.key_id
    {
        return Err(format!(
            "{LOCK_FILE} was not made against the snapshot beside it"
        ));
    }
    for locked in &lock.extensions {
        let listed = index
            .entry(&locked.id)
            .and_then(|entry| entry.versions.iter().find(|v| v.version == locked.version));
        if listed.is_none_or(|version| version.artifacts != locked.artifacts) {
            return Err(format!(
                "{LOCK_FILE}: {} {} is not in the snapshot as locked",
                locked.id, locked.version
            ));
        }
    }
    Ok(lock)
}

/// Where the lock and the snapshot ship: `registry` beside the executable. Tests use
/// the source tree.
pub fn shipped_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/registry"))
    }
    #[cfg(not(test))]
    {
        let executable = std::env::current_exe().ok()?;
        Some(crate::infrastructure::language_packs::bundled_directory(
            &executable,
            "registry",
        ))
    }
}

/// Where the index is cached under the extensions folder `root`.
pub fn cache_dir(root: &Path) -> PathBuf {
    root.join(".cache").join("registry")
}

/// Seconds since 1970.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn is_loopback(url: &str) -> bool {
    ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|prefix| url.starts_with(prefix))
}

/// The real transport: HTTPS (plain HTTP only to this machine, for tests), with the
/// conditions sent as `If-None-Match` and `If-Modified-Since`.
pub fn http(url: &str, conditions: &Conditions, limit: u64) -> Result<Fetched, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .user_agent(concat!("Loom/", env!("CARGO_PKG_VERSION")))
        .try_proxy_from_env(!is_loopback(url))
        .https_only(!is_loopback(url))
        .build();
    let mut request = agent.get(url);
    if let Some(etag) = &conditions.etag {
        request = request.set("If-None-Match", etag);
    }
    if let Some(date) = &conditions.last_modified {
        request = request.set("If-Modified-Since", date);
    }
    let response = request.call().map_err(|error| match error {
        ureq::Error::Status(code, _) => format!("the server returned HTTP {code}"),
        error => error.to_string(),
    })?;
    if response.status() == 304 {
        return Ok(Fetched::NotModified);
    }
    let header = |name: &str| response.header(name).map(str::to_owned);
    let (etag, last_modified) = (header("ETag"), header("Last-Modified"));
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "the answer from {url} is larger than {limit} bytes"
        ));
    }
    Ok(Fetched::Body {
        bytes,
        etag,
        last_modified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::cell::RefCell;

    struct Signer {
        pair: Ed25519KeyPair,
        public: String,
    }

    fn signer() -> Signer {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = BASE64.encode(pair.public_key().as_ref());
        Signer { pair, public }
    }

    fn index_text(sequence: u64, key_id: &str, expires: &str) -> String {
        format!(
            r#"{{ "schema": 1, "generated": "2026-11-01T00:00:00.000Z", "expires": "{expires}",
                "sequence": {sequence}, "keyId": "{key_id}",
                "extensions": [{{ "id": "loom.lang.rust", "name": "Rust", "versions": [
                  {{ "version": "1.0.0", "artifacts": [
                    {{ "platform": "any", "url": "https://x/r.loomext", "sha256": "{}", "size": 3 }}] }}] }}] }}"#,
            "b".repeat(64)
        )
    }

    fn sign(signer: &Signer, key_id: &str, bytes: &[u8]) -> Vec<u8> {
        serde_json::json!({
            "keyId": key_id, "algorithm": "ed25519",
            "signature": BASE64.encode(signer.pair.sign(bytes).as_ref()),
        })
        .to_string()
        .into_bytes()
    }

    const NOW: i64 = 1_793_491_200 + 86_400; // 2026-11-02
    const LATER: &str = "2026-12-01T00:00:00.000Z";

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loom-registry-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// A server that answers `index` and `signature`, honors the etag, and counts requests.
    struct Server {
        index: RefCell<Vec<u8>>,
        signature: RefCell<Vec<u8>>,
        etag: RefCell<String>,
        requests: RefCell<Vec<(String, Conditions)>>,
        down: RefCell<bool>,
    }

    impl Server {
        fn new(index: &str, signature: Vec<u8>) -> Self {
            Self {
                index: RefCell::new(index.as_bytes().to_vec()),
                signature: RefCell::new(signature),
                etag: RefCell::new("\"one\"".into()),
                requests: RefCell::new(Vec::new()),
                down: RefCell::new(false),
            }
        }
        fn get(&self, url: &str, conditions: &Conditions, _limit: u64) -> Result<Fetched, String> {
            self.requests
                .borrow_mut()
                .push((url.to_owned(), conditions.clone()));
            if *self.down.borrow() {
                return Err("offline".into());
            }
            if url.ends_with(".sig") {
                return Ok(Fetched::Body {
                    bytes: self.signature.borrow().clone(),
                    etag: None,
                    last_modified: None,
                });
            }
            if conditions.etag.as_deref() == Some(self.etag.borrow().as_str()) {
                return Ok(Fetched::NotModified);
            }
            Ok(Fetched::Body {
                bytes: self.index.borrow().clone(),
                etag: Some(self.etag.borrow().clone()),
                last_modified: None,
            })
        }
    }

    #[test]
    fn the_shipped_key_is_a_32_byte_ed25519_key() {
        for (id, key) in KEYS {
            assert!(!id.is_empty());
            assert_eq!(BASE64.decode(key).unwrap().len(), 32);
        }
    }

    #[test]
    fn a_signed_index_is_read_and_anything_else_is_refused() {
        let signer = signer();
        let keys = [("k1", signer.public.as_str())];
        let text = index_text(1, "k1", LATER);
        let good = sign(&signer, "k1", text.as_bytes());
        assert_eq!(verify(text.as_bytes(), &good, &keys).unwrap().sequence, 1);

        // One changed byte.
        let tampered = text.replace("\"sequence\": 1", "\"sequence\": 9");
        assert!(
            verify(tampered.as_bytes(), &good, &keys)
                .unwrap_err()
                .contains("does not match")
        );
        // A key Loom does not know, another algorithm, a signature that is not one.
        let unknown = sign(&signer, "k2", text.as_bytes());
        assert!(
            verify(text.as_bytes(), &unknown, &keys)
                .unwrap_err()
                .contains("unknown key")
        );
        let other = String::from_utf8(good.clone())
            .unwrap()
            .replace("ed25519", "rsa");
        assert!(
            verify(text.as_bytes(), other.as_bytes(), &keys)
                .unwrap_err()
                .contains("algorithm")
        );
        assert!(verify(text.as_bytes(), b"nope", &keys).is_err());
        // Another signer's key.
        let stranger = self::signer();
        let forged = sign(&stranger, "k1", text.as_bytes());
        assert!(verify(text.as_bytes(), &forged, &keys).is_err());
        // The index must name the key that signed it.
        let renamed = index_text(1, "k9", LATER);
        let signed = sign(&signer, "k1", renamed.as_bytes());
        assert!(
            verify(renamed.as_bytes(), &signed, &keys)
                .unwrap_err()
                .contains("different key")
        );
    }

    #[test]
    fn a_fetched_index_is_cached_and_asked_for_again_with_its_etag() {
        let signer = signer();
        let keys = [("k1", signer.public.as_str())];
        let text = index_text(1, "k1", LATER);
        let server = Server::new(&text, sign(&signer, "k1", text.as_bytes()));
        let dir = temp("cache");
        let get = |u: &str, c: &Conditions, l: u64| server.get(u, c, l);

        let first = current(&dir, None, "https://h", &keys, NOW, &get).unwrap();
        assert_eq!(
            (first.origin, first.expired, first.index.sequence),
            (Origin::Live, false, 1)
        );
        assert!(server.requests.borrow()[0].1 == Conditions::default());

        let again = current(&dir, None, "https://h", &keys, NOW + 60, &get).unwrap();
        assert_eq!(again.origin, Origin::Live);
        let requests = server.requests.borrow();
        let last_index = requests
            .iter()
            .rev()
            .find(|(url, _)| url.ends_with("index.json"))
            .unwrap();
        assert_eq!(last_index.1.etag.as_deref(), Some("\"one\""));
        assert_eq!(
            requests
                .iter()
                .filter(|(url, _)| url.ends_with(".sig"))
                .count(),
            1,
            "no new signature for an unchanged index"
        );
        drop(requests);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn when_the_server_cannot_be_reached_the_cached_index_is_used_and_says_why() {
        let signer = signer();
        let keys = [("k1", signer.public.as_str())];
        let text = index_text(1, "k1", LATER);
        let server = Server::new(&text, sign(&signer, "k1", text.as_bytes()));
        let dir = temp("offline");
        let get = |u: &str, c: &Conditions, l: u64| server.get(u, c, l);
        assert!(current(&dir, None, "https://h", &keys, NOW, &get).is_ok());
        *server.down.borrow_mut() = true;
        let cached = current(&dir, None, "https://h", &keys, NOW, &get).unwrap();
        assert_eq!(cached.origin, Origin::Cached);
        assert_eq!(cached.problem.as_deref(), Some("offline"));
        // With nothing cached there is nothing to fall back on.
        let empty = temp("offline-empty");
        assert_eq!(
            current(&empty, None, "https://h", &keys, NOW, &get).unwrap_err(),
            "offline"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_older_index_is_refused_even_when_it_is_signed_and_the_newer_one_stays() {
        let signer = signer();
        let keys = [("k1", signer.public.as_str())];
        let new = index_text(5, "k1", LATER);
        let server = Server::new(&new, sign(&signer, "k1", new.as_bytes()));
        let dir = temp("replay");
        let get = |u: &str, c: &Conditions, l: u64| server.get(u, c, l);
        assert_eq!(
            current(&dir, None, "https://h", &keys, NOW, &get)
                .unwrap()
                .index
                .sequence,
            5
        );

        // The server (or someone in between) serves an older, validly signed index.
        let old = index_text(4, "k1", LATER);
        *server.index.borrow_mut() = old.as_bytes().to_vec();
        *server.signature.borrow_mut() = sign(&signer, "k1", old.as_bytes());
        *server.etag.borrow_mut() = "\"two\"".into();
        let kept = current(&dir, None, "https://h", &keys, NOW, &get).unwrap();
        assert_eq!((kept.origin, kept.index.sequence), (Origin::Cached, 5));
        assert!(kept.problem.unwrap().contains("older"));

        // Even with the cache gone, the highest sequence seen is remembered.
        fs::remove_file(dir.join(INDEX_FILE)).unwrap();
        assert!(
            current(&dir, None, "https://h", &keys, NOW, &get)
                .unwrap_err()
                .contains("older")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    fn ship(dir: &Path, signer: &Signer, sequence: u64) -> String {
        let text = index_text(sequence, "k1", LATER);
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(SNAPSHOT_FILE), &text).unwrap();
        fs::write(
            dir.join(SNAPSHOT_SIGNATURE_FILE),
            sign(signer, "k1", text.as_bytes()),
        )
        .unwrap();
        text
    }

    #[test]
    fn the_shipped_snapshot_stands_in_when_nothing_else_is_available_and_the_newest_wins() {
        let signer = signer();
        let keys = [("k1", signer.public.as_str())];
        let shipped = temp("snapshot-shipped");
        ship(&shipped, &signer, 4);
        let dir = temp("snapshot");
        let down = Server::new("", Vec::new());
        *down.down.borrow_mut() = true;
        let get = |u: &str, c: &Conditions, l: u64| down.get(u, c, l);

        // Offline with an empty cache: the snapshot, with the reason the server was not used.
        let loaded = current(&dir, Some(&shipped), "https://h", &keys, NOW, &get).unwrap();
        assert_eq!(
            (loaded.origin, loaded.index.sequence),
            (Origin::Snapshot, 4)
        );
        assert_eq!(loaded.problem.as_deref(), Some("offline"));

        // A cache newer than the snapshot is preferred; an older one is not.
        let newer = index_text(6, "k1", LATER);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(INDEX_FILE), &newer).unwrap();
        fs::write(
            dir.join(SIGNATURE_FILE),
            sign(&signer, "k1", newer.as_bytes()),
        )
        .unwrap();
        let loaded = current(&dir, Some(&shipped), "https://h", &keys, NOW, &get).unwrap();
        assert_eq!((loaded.origin, loaded.index.sequence), (Origin::Cached, 6));
        let older = index_text(2, "k1", LATER);
        fs::write(dir.join(INDEX_FILE), &older).unwrap();
        fs::write(
            dir.join(SIGNATURE_FILE),
            sign(&signer, "k1", older.as_bytes()),
        )
        .unwrap();
        let loaded = current(&dir, Some(&shipped), "https://h", &keys, NOW, &get).unwrap();
        assert_eq!(
            (loaded.origin, loaded.index.sequence),
            (Origin::Snapshot, 4)
        );

        // A snapshot that does not verify is as good as none.
        fs::write(shipped.join(SNAPSHOT_FILE), index_text(9, "k1", LATER)).unwrap();
        assert!(load_snapshot(&shipped, &keys, NOW).is_none());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&shipped);
    }

    #[test]
    fn the_snapshot_sets_the_floor_for_a_live_index() {
        let signer = signer();
        let keys = [("k1", signer.public.as_str())];
        let shipped = temp("floor-shipped");
        ship(&shipped, &signer, 5);
        let dir = temp("floor");
        // The server answers with a validly signed index older than what shipped.
        let stale = index_text(3, "k1", LATER);
        let server = Server::new(&stale, sign(&signer, "k1", stale.as_bytes()));
        let get = |u: &str, c: &Conditions, l: u64| server.get(u, c, l);
        let loaded = current(&dir, Some(&shipped), "https://h", &keys, NOW, &get).unwrap();
        assert_eq!(
            (loaded.origin, loaded.index.sequence),
            (Origin::Snapshot, 5)
        );
        assert!(loaded.problem.unwrap().contains("older"));
        assert!(!dir.join(INDEX_FILE).exists(), "the replay was not kept");
        // A newer one is taken.
        let fresh = index_text(7, "k1", LATER);
        *server.index.borrow_mut() = fresh.as_bytes().to_vec();
        *server.signature.borrow_mut() = sign(&signer, "k1", fresh.as_bytes());
        let loaded = current(&dir, Some(&shipped), "https://h", &keys, NOW, &get).unwrap();
        assert_eq!((loaded.origin, loaded.index.sequence), (Origin::Live, 7));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&shipped);
    }

    #[test]
    fn a_lock_must_match_the_snapshot_it_ships_with() {
        let signer = signer();
        let keys = [("k1", signer.public.as_str())];
        let dir = temp("lock");
        let text = ship(&dir, &signer, 2);
        let good = serde_json::json!({
            "schema": 1,
            "index": { "sequence": 2, "keyId": "k1", "sha256": super::super::package::sha256(text.as_bytes()) },
            "extensions": [{ "id": "loom.lang.rust", "version": "1.0.0", "artifacts": [
                { "platform": "any", "url": "https://x/r.loomext", "sha256": "b".repeat(64), "size": 3 }] }],
        });
        let write =
            |lock: &serde_json::Value| fs::write(dir.join(LOCK_FILE), lock.to_string()).unwrap();
        write(&good);
        let lock = load_lock(&dir, &keys).unwrap();
        assert_eq!(
            (lock.index.sequence, lock.extensions[0].id.as_str()),
            (2, "loom.lang.rust")
        );

        for (path, value, expected) in [
            (
                "/index/sha256",
                serde_json::json!("0".repeat(64)),
                "not made against",
            ),
            ("/index/sequence", serde_json::json!(9), "not made against"),
            (
                "/extensions/0/version",
                serde_json::json!("9.9.9"),
                "not in the snapshot",
            ),
            (
                "/extensions/0/artifacts/0/size",
                serde_json::json!(4),
                "not in the snapshot",
            ),
            (
                "/extensions/0/id",
                serde_json::json!("loom.lang.zig"),
                "not in the snapshot",
            ),
            ("/schema", serde_json::json!(2), "unsupported schema"),
        ] {
            let mut bad = good.clone();
            *bad.pointer_mut(path).unwrap() = value;
            write(&bad);
            let error = load_lock(&dir, &keys).unwrap_err();
            assert!(error.contains(expected), "{path}: {error}");
        }
        // A snapshot signed by someone else is refused even if the lock matches it.
        write(&good);
        fs::write(
            dir.join(SNAPSHOT_SIGNATURE_FILE),
            sign(&self::signer(), "k1", text.as_bytes()),
        )
        .unwrap();
        assert!(load_lock(&dir, &keys).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// What ships in assets/registry is a lock and snapshot that belong together,
    /// signed by a key this build trusts.
    #[test]
    fn the_lock_and_snapshot_that_ship_with_loom_belong_together() {
        let Some(dir) = shipped_dir().filter(|dir| dir.join(LOCK_FILE).is_file()) else {
            return;
        };
        let lock = load_lock(&dir, KEYS).unwrap();
        assert!(!lock.extensions.is_empty());
        assert!(load_snapshot(&dir, KEYS, 0).is_some());
    }

    #[test]
    fn an_expired_index_is_kept_but_marked() {
        let signer = signer();
        let keys = [("k1", signer.public.as_str())];
        let text = index_text(1, "k1", "2026-11-05T00:00:00.000Z");
        let server = Server::new(&text, sign(&signer, "k1", text.as_bytes()));
        let dir = temp("expired");
        let get = |u: &str, c: &Conditions, l: u64| server.get(u, c, l);
        let week_later = NOW + 7 * 86_400;
        let loaded = current(&dir, None, "https://h", &keys, week_later, &get).unwrap();
        assert!(loaded.expired);
        assert!(!load_cached(&dir, &keys, NOW).unwrap().expired);
        assert!(load_cached(&dir, &keys, week_later).unwrap().expired);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_or_swapped_cache_is_as_good_as_none() {
        let signer = signer();
        let keys = [("k1", signer.public.as_str())];
        let text = index_text(1, "k1", LATER);
        let server = Server::new(&text, sign(&signer, "k1", text.as_bytes()));
        let dir = temp("damaged");
        let get = |u: &str, c: &Conditions, l: u64| server.get(u, c, l);
        current(&dir, None, "https://h", &keys, NOW, &get).unwrap();
        assert!(load_cached(&dir, &keys, NOW).is_some());
        fs::write(dir.join(INDEX_FILE), text.replace("Rust", "Evil")).unwrap();
        assert!(load_cached(&dir, &keys, NOW).is_none());
        // With the cache unusable the server's answer is fetched whole, not "not modified".
        let again = current(&dir, None, "https://h", &keys, NOW, &get).unwrap();
        assert_eq!(again.origin, Origin::Live);
        let _ = fs::remove_dir_all(&dir);
    }
}
