//! API keys and per-caller quotas.
//!
//! Callers are listed in a JSON file (a Kubernetes Secret on the cluster)
//! that holds only SHA-256 hashes of their keys, never the keys:
//!
//! ```json
//! { "callers": [
//!     { "id": "lab-01", "key_sha256": "<64 hex characters>" },
//!     { "id": "loadtest", "key_sha256": "...", "synthetic": true,
//!       "residues_per_minute": 10000000, "max_inflight_tokens": 16384 } ] }
//! ```
//!
//! `id` is a pseudonym chosen by the operator; it appears in metrics and
//! logs, so it must not be a person's name. Which person holds which id is
//! kept outside the platform. `synthetic` marks test traffic, which the SLOs
//! leave out.
//!
//! Each caller has two limits, both answered with 429:
//! - a **rate** in residues per minute (a token bucket that holds up to one
//!   minute's allowance, so short bursts are fine), because cost follows
//!   residues, not requests;
//! - a **share of the queue**: tokens of the caller's requests in progress,
//!   so one heavy caller can't fill the shared queue and push everyone else
//!   into 503s.
//!
//! The file is re-read periodically: adding or revoking a key needs no
//! restart. A missing file means no callers yet; a malformed one is
//! rejected and the previous list stays in force.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Limits applied to callers that don't set their own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Defaults {
    pub residues_per_minute: u64,
    pub max_inflight_tokens: usize,
    /// The largest request the gateway accepts, in residues and in tokens;
    /// a caller whose limits are smaller could never be served.
    pub largest_request_residues: u64,
    pub largest_request_tokens: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Limits {
    residues_per_minute: u64,
    max_inflight_tokens: usize,
}

#[derive(Debug)]
struct Bucket {
    /// Residues that may still be spent.
    available: f64,
    updated: Instant,
}

/// An authenticated caller and its quota state.
#[derive(Debug)]
pub struct Caller {
    pub id: String,
    /// Test traffic: counted separately and left out of the SLOs.
    pub synthetic: bool,
    key_sha256: [u8; 32],
    limits: Limits,
    bucket: Mutex<Bucket>,
    inflight: AtomicUsize,
}

/// Why a caller's request was refused, and when to retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    RateLimited { retry_after_secs: u64 },
    TooMuchInFlight,
}

impl Caller {
    fn new(id: String, key_sha256: [u8; 32], limits: Limits, synthetic: bool) -> Self {
        Self {
            id,
            synthetic,
            key_sha256,
            limits,
            // Start full: a new caller may use a minute's allowance at once.
            bucket: Mutex::new(Bucket {
                available: limits.residues_per_minute as f64,
                updated: Instant::now(),
            }),
            inflight: AtomicUsize::new(0),
        }
    }

    /// An unauthenticated caller, when the gateway allows anonymous use.
    pub fn anonymous(defaults: &Defaults) -> Arc<Self> {
        Arc::new(Self::new(
            "anonymous".into(),
            [0; 32],
            Limits {
                residues_per_minute: defaults.residues_per_minute,
                max_inflight_tokens: defaults.max_inflight_tokens,
            },
            false,
        ))
    }

    fn per_second(&self) -> f64 {
        self.limits.residues_per_minute as f64 / 60.0
    }

    /// Spends `residues` from the caller's allowance at time `now`, or says
    /// how long until enough has refilled.
    pub fn take(&self, residues: u64, now: Instant) -> Result<(), Refusal> {
        let mut b = self.bucket.lock().expect("bucket lock poisoned");
        let elapsed = now.saturating_duration_since(b.updated).as_secs_f64();
        b.available =
            (b.available + elapsed * self.per_second()).min(self.limits.residues_per_minute as f64);
        b.updated = now;
        let need = residues as f64;
        if b.available >= need {
            b.available -= need;
            Ok(())
        } else {
            let wait = (need - b.available) / self.per_second();
            Err(Refusal::RateLimited {
                retry_after_secs: wait.ceil().max(1.0) as u64,
            })
        }
    }

    /// Gives back residues taken for a request that was then not run.
    pub fn refund(&self, residues: u64) {
        let mut b = self.bucket.lock().expect("bucket lock poisoned");
        b.available = (b.available + residues as f64).min(self.limits.residues_per_minute as f64);
    }

    /// Reserves the caller's share of the queue for a request; released when
    /// the guard is dropped (the request finished, failed or was abandoned).
    pub fn reserve(self: &Arc<Self>, tokens: usize) -> Result<InFlight, Refusal> {
        let max = self.limits.max_inflight_tokens;
        self.inflight
            .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held + tokens <= max).then_some(held + tokens)
            })
            .map(|_| InFlight {
                caller: self.clone(),
                tokens,
            })
            .map_err(|_| Refusal::TooMuchInFlight)
    }

    pub fn inflight_tokens(&self) -> usize {
        self.inflight.load(Ordering::Acquire)
    }
}

/// A caller's reservation for one request in progress.
#[derive(Debug)]
pub struct InFlight {
    caller: Arc<Caller>,
    tokens: usize,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.caller
            .inflight
            .fetch_sub(self.tokens, Ordering::AcqRel);
    }
}

pub fn sha256(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

// --- the callers file ------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCaller {
    id: String,
    key_sha256: String,
    residues_per_minute: Option<u64>,
    max_inflight_tokens: Option<usize>,
    #[serde(default)]
    synthetic: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallersFile {
    callers: Vec<FileCaller>,
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b))
}

/// One checked entry of the callers file.
struct Entry {
    id: String,
    hash: [u8; 32],
    limits: Limits,
    synthetic: bool,
}

/// Parses and checks a callers file. Errors name the entry, never a key hash.
fn parse(bytes: &[u8], defaults: &Defaults) -> Result<Vec<Entry>, String> {
    let file: CallersFile =
        serde_json::from_slice(bytes).map_err(|e| format!("not a valid callers file: {e}"))?;
    let mut ids = std::collections::HashSet::new();
    let mut hashes = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(file.callers.len());
    for (i, c) in file.callers.into_iter().enumerate() {
        if !valid_id(&c.id) {
            return Err(format!(
                "caller {i}: id must be 1-64 lowercase letters, digits, '-', '_' or '.'"
            ));
        }
        let hash = parse_hex32(&c.key_sha256)
            .ok_or_else(|| format!("caller {}: key_sha256 must be 64 hex characters", c.id))?;
        if !ids.insert(c.id.clone()) {
            return Err(format!("caller {}: listed twice", c.id));
        }
        if !hashes.insert(hash) {
            return Err(format!("caller {}: shares a key with another caller", c.id));
        }
        let limits = Limits {
            residues_per_minute: c
                .residues_per_minute
                .unwrap_or(defaults.residues_per_minute),
            max_inflight_tokens: c
                .max_inflight_tokens
                .unwrap_or(defaults.max_inflight_tokens),
        };
        if limits.residues_per_minute < defaults.largest_request_residues {
            return Err(format!(
                "caller {}: residues_per_minute ({}) is below the largest allowed request ({})",
                c.id, limits.residues_per_minute, defaults.largest_request_residues
            ));
        }
        if limits.max_inflight_tokens < defaults.largest_request_tokens {
            return Err(format!(
                "caller {}: max_inflight_tokens ({}) is below the largest allowed request ({} tokens)",
                c.id, limits.max_inflight_tokens, defaults.largest_request_tokens
            ));
        }
        out.push(Entry {
            id: c.id,
            hash,
            limits,
            synthetic: c.synthetic,
        });
    }
    Ok(out)
}

/// The current list of callers, reloadable from a file.
pub struct Callers {
    path: Option<PathBuf>,
    defaults: Defaults,
    by_hash: RwLock<Arc<HashMap<[u8; 32], Arc<Caller>>>>,
    last: Mutex<Option<Vec<u8>>>,
}

/// What a reload did.
#[derive(Debug, PartialEq, Eq)]
pub enum Reloaded {
    Unchanged,
    Updated { callers: usize },
}

impl Callers {
    pub fn new(path: Option<PathBuf>, defaults: Defaults) -> Self {
        Self {
            path,
            defaults,
            by_hash: RwLock::new(Arc::new(HashMap::new())),
            last: Mutex::new(None),
        }
    }

    pub fn defaults(&self) -> &Defaults {
        &self.defaults
    }

    pub fn count(&self) -> usize {
        self.by_hash.read().expect("callers lock poisoned").len()
    }

    /// Re-reads the file if it changed. A missing file means no callers; on
    /// an invalid file the previous list stays in force.
    pub fn reload(&self) -> Result<Reloaded, String> {
        let Some(path) = &self.path else {
            return Ok(Reloaded::Unchanged);
        };
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("can't read {}: {e}", path.display())),
        };
        let mut last = self.last.lock().expect("reload lock poisoned");
        if last.as_deref() == Some(&bytes[..]) {
            return Ok(Reloaded::Unchanged);
        }
        let entries = if bytes.is_empty() {
            Vec::new()
        } else {
            parse(&bytes, &self.defaults)?
        };

        // Keep the quota state of callers whose entry didn't change.
        let old = self.by_hash.read().expect("callers lock poisoned").clone();
        let by_id: HashMap<&str, &Arc<Caller>> = old.values().map(|c| (c.id.as_str(), c)).collect();
        let mut next = HashMap::with_capacity(entries.len());
        for Entry {
            id,
            hash,
            limits,
            synthetic,
        } in entries
        {
            let caller = match by_id.get(id.as_str()) {
                Some(c)
                    if c.key_sha256 == hash && c.limits == limits && c.synthetic == synthetic =>
                {
                    Arc::clone(c)
                }
                _ => Arc::new(Caller::new(id, hash, limits, synthetic)),
            };
            next.insert(hash, caller);
        }
        let callers = next.len();
        *self.by_hash.write().expect("callers lock poisoned") = Arc::new(next);
        *last = Some(bytes);
        Ok(Reloaded::Updated { callers })
    }

    /// The caller holding `key`, if any. The lookup is by the key's hash, so
    /// no comparison ever touches the key itself.
    pub fn authenticate(&self, key: &str) -> Option<Arc<Caller>> {
        self.by_hash
            .read()
            .expect("callers lock poisoned")
            .get(&sha256(key))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    const DEFAULTS: Defaults = Defaults {
        residues_per_minute: 600,
        max_inflight_tokens: 300,
        largest_request_residues: 100,
        largest_request_tokens: 120,
    };

    fn hex(h: [u8; 32]) -> String {
        h.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn file(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn callers_json(entries: &[(&str, &str, &str)]) -> String {
        let list: Vec<String> = entries
            .iter()
            .map(|(id, key, extra)| {
                format!(
                    r#"{{"id":"{id}","key_sha256":"{}"{extra}}}"#,
                    hex(sha256(key))
                )
            })
            .collect();
        format!(r#"{{"callers":[{}]}}"#, list.join(","))
    }

    #[test]
    fn authenticates_by_key_hash() {
        let f = file(&callers_json(&[
            ("lab-01", "secret-a", ""),
            ("loadtest", "secret-b", r#","synthetic":true"#),
        ]));
        let c = Callers::new(Some(f.path().into()), DEFAULTS);
        assert_eq!(c.reload(), Ok(Reloaded::Updated { callers: 2 }));
        assert_eq!(c.authenticate("secret-a").unwrap().id, "lab-01");
        assert!(c.authenticate("secret-b").unwrap().synthetic);
        assert!(c.authenticate("secret-c").is_none());
        assert!(c.authenticate("").is_none());
    }

    #[test]
    fn missing_file_means_no_callers() {
        let c = Callers::new(Some("/nonexistent/callers.json".into()), DEFAULTS);
        assert_eq!(c.reload(), Ok(Reloaded::Updated { callers: 0 }));
        assert_eq!(c.count(), 0);
    }

    #[test]
    fn invalid_files_are_rejected_and_the_old_list_stays() {
        let f = file(&callers_json(&[("lab-01", "secret-a", "")]));
        let c = Callers::new(Some(f.path().into()), DEFAULTS);
        c.reload().unwrap();
        let bad = [
            r#"{"callers":[{"id":"Lab 01","key_sha256":"00"}]}"#.to_string(),
            r#"{"callers":[{"id":"x","key_sha256":"zz"}]}"#.to_string(),
            callers_json(&[("a", "k", ""), ("a", "k2", "")]),
            callers_json(&[("a", "k", ""), ("b", "k", "")]),
            callers_json(&[("a", "k", r#","residues_per_minute":50"#)]),
            callers_json(&[("a", "k", r#","max_inflight_tokens":100"#)]),
            r#"{"callers":[{"id":"a","key_sha256":"","admin":true}]}"#.to_string(),
            "not json".to_string(),
        ];
        for content in bad {
            std::fs::write(f.path(), &content).unwrap();
            let err = c.reload().unwrap_err();
            assert!(
                !err.contains(&hex(sha256("k"))),
                "errors never echo a key hash: {err}"
            );
            assert_eq!(
                c.authenticate("secret-a").unwrap().id,
                "lab-01",
                "old list kept after: {content}"
            );
        }
    }

    #[test]
    fn revoking_a_key_takes_effect_on_reload() {
        let f = file(&callers_json(&[
            ("lab-01", "secret-a", ""),
            ("lab-02", "secret-b", ""),
        ]));
        let c = Callers::new(Some(f.path().into()), DEFAULTS);
        c.reload().unwrap();
        std::fs::write(f.path(), callers_json(&[("lab-02", "secret-b", "")])).unwrap();
        assert_eq!(c.reload(), Ok(Reloaded::Updated { callers: 1 }));
        assert!(c.authenticate("secret-a").is_none());
        assert_eq!(c.reload(), Ok(Reloaded::Unchanged));
    }

    #[test]
    fn reload_keeps_quota_state_of_unchanged_callers() {
        let f = file(&callers_json(&[("lab-01", "secret-a", "")]));
        let c = Callers::new(Some(f.path().into()), DEFAULTS);
        c.reload().unwrap();
        let now = Instant::now();
        c.authenticate("secret-a").unwrap().take(600, now).unwrap();
        // Another caller added: lab-01's spent allowance must not reset.
        std::fs::write(
            f.path(),
            callers_json(&[("lab-01", "secret-a", ""), ("lab-02", "secret-b", "")]),
        )
        .unwrap();
        c.reload().unwrap();
        assert!(c.authenticate("secret-a").unwrap().take(1, now).is_err());
    }

    #[test]
    fn rate_limit_is_a_token_bucket_in_residues() {
        let caller = Caller::new(
            "a".into(),
            [1; 32],
            Limits {
                residues_per_minute: 600,
                max_inflight_tokens: 300,
            },
            false,
        );
        let t0 = Instant::now();
        // A full minute's allowance is available at once.
        assert_eq!(caller.take(600, t0), Ok(()));
        // Empty: 10 residues per second refill, so 50 residues need 5 s.
        assert_eq!(
            caller.take(50, t0),
            Err(Refusal::RateLimited {
                retry_after_secs: 5
            })
        );
        assert_eq!(caller.take(50, t0 + Duration::from_secs(5)), Ok(()));
        // Never more than one minute's allowance, however long it was idle.
        assert_eq!(
            caller.take(601, t0 + Duration::from_secs(3600)),
            Err(Refusal::RateLimited {
                retry_after_secs: 1
            })
        );
        assert_eq!(caller.take(600, t0 + Duration::from_secs(3600)), Ok(()));
        caller.refund(100);
        assert_eq!(caller.take(100, t0 + Duration::from_secs(3600)), Ok(()));
    }

    #[test]
    fn queue_share_is_held_until_the_request_ends() {
        let caller = Arc::new(Caller::new(
            "a".into(),
            [1; 32],
            Limits {
                residues_per_minute: 600,
                max_inflight_tokens: 300,
            },
            false,
        ));
        let first = caller.reserve(200).unwrap();
        assert_eq!(caller.reserve(101).unwrap_err(), Refusal::TooMuchInFlight);
        let second = caller.reserve(100).unwrap();
        assert_eq!(caller.inflight_tokens(), 300);
        drop(first);
        assert_eq!(caller.inflight_tokens(), 100);
        drop(second);
        assert_eq!(caller.inflight_tokens(), 0);
    }
}
