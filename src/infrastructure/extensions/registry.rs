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
use crate::features::extensions::registry::{Index, check_sequence};
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

/// The index to use: a fresh one when the server gives a good one, else the cached
/// copy (with the reason), else an error.
pub fn current(
    dir: &Path,
    base: &str,
    keys: &[(&str, &str)],
    now: i64,
    transport: &Transport,
) -> Result<Loaded, String> {
    match refresh(dir, base, keys, now, transport) {
        Ok(loaded) => Ok(loaded),
        Err(problem) => match load_cached(dir, keys, now) {
            Some(mut cached) => {
                cached.problem = Some(problem);
                Ok(cached)
            }
            None => Err(problem),
        },
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

        let first = current(&dir, "https://h", &keys, NOW, &get).unwrap();
        assert_eq!(
            (first.origin, first.expired, first.index.sequence),
            (Origin::Live, false, 1)
        );
        assert!(server.requests.borrow()[0].1 == Conditions::default());

        let again = current(&dir, "https://h", &keys, NOW + 60, &get).unwrap();
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
        assert!(current(&dir, "https://h", &keys, NOW, &get).is_ok());
        *server.down.borrow_mut() = true;
        let cached = current(&dir, "https://h", &keys, NOW, &get).unwrap();
        assert_eq!(cached.origin, Origin::Cached);
        assert_eq!(cached.problem.as_deref(), Some("offline"));
        // With nothing cached there is nothing to fall back on.
        let empty = temp("offline-empty");
        assert_eq!(
            current(&empty, "https://h", &keys, NOW, &get).unwrap_err(),
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
            current(&dir, "https://h", &keys, NOW, &get)
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
        let kept = current(&dir, "https://h", &keys, NOW, &get).unwrap();
        assert_eq!((kept.origin, kept.index.sequence), (Origin::Cached, 5));
        assert!(kept.problem.unwrap().contains("older"));

        // Even with the cache gone, the highest sequence seen is remembered.
        fs::remove_file(dir.join(INDEX_FILE)).unwrap();
        assert!(
            current(&dir, "https://h", &keys, NOW, &get)
                .unwrap_err()
                .contains("older")
        );
        let _ = fs::remove_dir_all(&dir);
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
        let loaded = current(&dir, "https://h", &keys, week_later, &get).unwrap();
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
        current(&dir, "https://h", &keys, NOW, &get).unwrap();
        assert!(load_cached(&dir, &keys, NOW).is_some());
        fs::write(dir.join(INDEX_FILE), text.replace("Rust", "Evil")).unwrap();
        assert!(load_cached(&dir, &keys, NOW).is_none());
        // With the cache unusable the server's answer is fetched whole, not "not modified".
        let again = current(&dir, "https://h", &keys, NOW, &get).unwrap();
        assert_eq!(again.origin, Origin::Live);
        let _ = fs::remove_dir_all(&dir);
    }
}
