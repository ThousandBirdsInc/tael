//! Server-managed eval case suites.
//!
//! `tael eval run` takes a JSONL file, which makes the local filesystem the
//! system of record: two people running "the same suite" may not be, and an
//! experiment comparison between two runs can silently compare different data.
//! This module makes the server the system of record while keeping JSONL as
//! the interchange format, so suites still live in git if that is what a team
//! wants.
//!
//! A case is stored as a content-addressed blob, so a snapshot is just an
//! ordered list of hashes. That makes three things fall out for free:
//! unchanged cases are shared between snapshots rather than copied, diffing
//! two snapshots is a set operation on hashes, and a case's identity is its
//! content — an edited case is a different case, which is exactly what you
//! want when a run's results are attributed to it.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SUITES_FILE: &str = "eval_suites.json";

/// One case: its stable id and the hash of its canonical content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseRef {
    pub case_id: String,
    /// SHA-256 of the canonical JSON. Two cases with the same id but different
    /// content have different hashes, which is how an edit is detected.
    pub content_sha256: String,
}

/// An immutable point-in-time capture of a suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub cases: Vec<CaseRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Snapshot {
    pub fn case_count(&self) -> usize {
        self.cases.len()
    }
}

/// A suite: its current working set plus every snapshot taken of it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Suite {
    #[serde(default)]
    pub cases: Vec<CaseRef>,
    #[serde(default)]
    pub snapshots: Vec<Snapshot>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SuitesFile {
    #[serde(default)]
    suites: BTreeMap<String, Suite>,
}

/// What changed between two case sets.
#[derive(Debug, Clone, Serialize)]
pub struct SuiteDiff {
    /// Cases present only on the right side.
    pub added: Vec<CaseRef>,
    /// Cases present only on the left side.
    pub removed: Vec<CaseRef>,
    /// Same `case_id`, different content.
    pub changed: Vec<ChangedCase>,
    pub unchanged: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangedCase {
    pub case_id: String,
    pub from_sha256: String,
    pub to_sha256: String,
}

pub struct SuiteStore {
    data_dir: String,
    suites: RwLock<BTreeMap<String, Suite>>,
}

impl SuiteStore {
    pub fn open(data_dir: &str) -> Result<Self> {
        Ok(Self {
            data_dir: data_dir.to_string(),
            suites: RwLock::new(Self::load(data_dir)?),
        })
    }

    pub fn path(data_dir: &str) -> PathBuf {
        Path::new(data_dir).join(SUITES_FILE)
    }

    fn load(data_dir: &str) -> Result<BTreeMap<String, Suite>> {
        match std::fs::read(Self::path(data_dir)) {
            Ok(bytes) => Ok(serde_json::from_slice::<SuitesFile>(&bytes)
                .context("parsing eval suites")?
                .suites),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e).context("reading eval suites"),
        }
    }

    fn save(&self) -> Result<()> {
        let suites = self.suites.read().expect("suites poisoned").clone();
        let path = Self::path(&self.data_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&SuitesFile { suites })?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn list(&self) -> BTreeMap<String, Suite> {
        self.suites.read().expect("suites poisoned").clone()
    }

    pub fn get(&self, name: &str) -> Option<Suite> {
        self.suites
            .read()
            .expect("suites poisoned")
            .get(name)
            .cloned()
    }

    /// Replace a suite's working set with `cases`.
    ///
    /// Push is a whole-set replace rather than a merge, because a merge makes
    /// deleting a case impossible without a second verb, and a suite whose
    /// stale cases cannot be removed stops being a description of what should
    /// pass.
    pub fn push(&self, name: &str, cases: Vec<CaseRef>) -> Result<usize> {
        if name.trim().is_empty() {
            bail!("a suite needs a name");
        }
        let mut seen = HashMap::new();
        for case in &cases {
            if let Some(previous) = seen.insert(&case.case_id, &case.content_sha256)
                && previous != &case.content_sha256
            {
                bail!(
                    "case id `{}` appears twice with different content; \
                     ids must be unique within a suite",
                    case.case_id
                );
            }
        }

        let count = cases.len();
        let mut suites = self.suites.write().expect("suites poisoned");
        let suite = suites.entry(name.to_string()).or_default();
        suite.cases = cases;
        suite.updated_at = Some(Utc::now());
        drop(suites);
        self.save()?;
        Ok(count)
    }

    /// Freeze the current working set as an immutable snapshot.
    pub fn snapshot(&self, name: &str, note: Option<String>) -> Result<Snapshot> {
        let mut suites = self.suites.write().expect("suites poisoned");
        let Some(suite) = suites.get_mut(name) else {
            bail!("no suite named `{name}`");
        };
        if suite.cases.is_empty() {
            bail!("suite `{name}` has no cases to snapshot");
        }
        // Derived from content, so an identical working set always produces the
        // same id — re-snapshotting an unchanged suite is a no-op rather than
        // an accumulating pile of duplicates.
        let id = snapshot_id(&suite.cases);
        if let Some(existing) = suite.snapshots.iter().find(|s| s.id == id) {
            return Ok(existing.clone());
        }
        let snapshot = Snapshot {
            id,
            created_at: Utc::now(),
            cases: suite.cases.clone(),
            note,
        };
        suite.snapshots.push(snapshot.clone());
        drop(suites);
        self.save()?;
        Ok(snapshot)
    }

    /// Resolve a `suite` or `suite@snapshot` reference to its case list.
    pub fn resolve(&self, reference: &str) -> Result<(String, Option<String>, Vec<CaseRef>)> {
        let (name, snapshot_id) = match reference.split_once('@') {
            Some((n, s)) => (n, Some(s)),
            None => (reference, None),
        };
        let Some(suite) = self.get(name) else {
            bail!("no suite named `{name}`");
        };
        match snapshot_id {
            None => Ok((name.to_string(), None, suite.cases)),
            Some(id) => {
                let snapshot = suite
                    .snapshots
                    .iter()
                    .find(|s| s.id == id || s.id.starts_with(id))
                    .ok_or_else(|| anyhow::anyhow!("suite `{name}` has no snapshot `{id}`"))?;
                Ok((
                    name.to_string(),
                    Some(snapshot.id.clone()),
                    snapshot.cases.clone(),
                ))
            }
        }
    }

    /// Compare two suite references.
    pub fn diff(&self, left: &str, right: &str) -> Result<SuiteDiff> {
        let (_, _, from) = self.resolve(left)?;
        let (_, _, to) = self.resolve(right)?;
        Ok(diff_cases(&from, &to))
    }
}

/// Set-difference two case lists by id, treating a content change as neither
/// an add nor a remove — an edited case is still the same case for the purpose
/// of understanding what a suite now covers.
pub fn diff_cases(from: &[CaseRef], to: &[CaseRef]) -> SuiteDiff {
    let left: HashMap<&str, &str> = from
        .iter()
        .map(|c| (c.case_id.as_str(), c.content_sha256.as_str()))
        .collect();
    let right: HashMap<&str, &str> = to
        .iter()
        .map(|c| (c.case_id.as_str(), c.content_sha256.as_str()))
        .collect();

    let mut diff = SuiteDiff {
        added: Vec::new(),
        removed: Vec::new(),
        changed: Vec::new(),
        unchanged: 0,
    };

    for case in to {
        match left.get(case.case_id.as_str()) {
            None => diff.added.push(case.clone()),
            Some(previous) if *previous != case.content_sha256 => {
                diff.changed.push(ChangedCase {
                    case_id: case.case_id.clone(),
                    from_sha256: previous.to_string(),
                    to_sha256: case.content_sha256.clone(),
                });
            }
            Some(_) => diff.unchanged += 1,
        }
    }
    for case in from {
        if !right.contains_key(case.case_id.as_str()) {
            diff.removed.push(case.clone());
        }
    }
    diff
}

/// Canonicalize a case so the same logical case always hashes the same way.
///
/// JSON object key order is not significant but is preserved by most writers,
/// so two byte-different files can hold identical cases. Serializing through a
/// sorted map removes that difference; without it, reformatting a suite file
/// would look like every case changed.
pub fn canonical_case_json(value: &serde_json::Value) -> Result<String> {
    fn sort(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted: BTreeMap<_, _> =
                    map.iter().map(|(k, v)| (k.clone(), sort(v))).collect();
                serde_json::to_value(sorted).unwrap_or(serde_json::Value::Null)
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(sort).collect())
            }
            other => other.clone(),
        }
    }
    serde_json::to_string(&sort(value)).context("canonicalizing case")
}

pub fn case_hash(canonical: &str) -> String {
    hex::encode(Sha256::digest(canonical.as_bytes()))
}

/// A snapshot's id: the hash of its ordered case hashes, truncated for
/// readability. Content-derived so it is stable and comparable.
fn snapshot_id(cases: &[CaseRef]) -> String {
    let mut hasher = Sha256::new();
    for case in cases {
        hasher.update(case.case_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(case.content_sha256.as_bytes());
        hasher.update(b"\n");
    }
    format!("snap_{}", &hex::encode(hasher.finalize())[..12])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn case(id: &str, input: &str) -> (CaseRef, String) {
        let value = json!({ "case_id": id, "input": input });
        let canonical = canonical_case_json(&value).unwrap();
        let hash = case_hash(&canonical);
        (
            CaseRef {
                case_id: id.to_string(),
                content_sha256: hash,
            },
            canonical,
        )
    }

    fn refs(pairs: &[(&str, &str)]) -> Vec<CaseRef> {
        pairs.iter().map(|(id, input)| case(id, input).0).collect()
    }

    #[test]
    fn canonicalization_ignores_key_order() {
        // Otherwise reformatting a suite file would look like every case
        // changed, and every snapshot diff would be noise.
        let a = canonical_case_json(&json!({ "b": 1, "a": 2 })).unwrap();
        let b = canonical_case_json(&json!({ "a": 2, "b": 1 })).unwrap();
        assert_eq!(a, b);
        assert_eq!(case_hash(&a), case_hash(&b));
    }

    #[test]
    fn canonicalization_preserves_array_order() {
        // Arrays are ordered data, unlike object keys.
        let a = canonical_case_json(&json!({ "steps": [1, 2] })).unwrap();
        let b = canonical_case_json(&json!({ "steps": [2, 1] })).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn push_replaces_the_working_set() {
        let dir = tempfile::tempdir().unwrap();
        let store = SuiteStore::open(dir.path().to_str().unwrap()).unwrap();

        store
            .push("regression", refs(&[("a", "1"), ("b", "2")]))
            .unwrap();
        assert_eq!(store.get("regression").unwrap().cases.len(), 2);

        // A push that omits a case removes it — otherwise a stale case could
        // never be deleted.
        store.push("regression", refs(&[("a", "1")])).unwrap();
        let cases = store.get("regression").unwrap().cases;
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case_id, "a");
    }

    #[test]
    fn push_rejects_a_duplicate_id_with_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = SuiteStore::open(dir.path().to_str().unwrap()).unwrap();
        let err = store
            .push("dupes", refs(&[("a", "1"), ("a", "2")]))
            .unwrap_err();
        assert!(err.to_string().contains("appears twice"), "{err}");
    }

    #[test]
    fn snapshots_are_content_derived_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = SuiteStore::open(dir.path().to_str().unwrap()).unwrap();
        store.push("s", refs(&[("a", "1"), ("b", "2")])).unwrap();

        let first = store.snapshot("s", None).unwrap();
        let again = store.snapshot("s", None).unwrap();
        assert_eq!(
            first.id, again.id,
            "an unchanged suite re-snapshots to itself"
        );
        assert_eq!(store.get("s").unwrap().snapshots.len(), 1);

        // Changing the working set produces a different snapshot.
        store
            .push("s", refs(&[("a", "1"), ("b", "CHANGED")]))
            .unwrap();
        let third = store.snapshot("s", None).unwrap();
        assert_ne!(first.id, third.id);
        assert_eq!(store.get("s").unwrap().snapshots.len(), 2);
    }

    #[test]
    fn a_snapshot_is_immutable_once_taken() {
        let dir = tempfile::tempdir().unwrap();
        let store = SuiteStore::open(dir.path().to_str().unwrap()).unwrap();
        store.push("s", refs(&[("a", "1")])).unwrap();
        let snap = store.snapshot("s", None).unwrap();

        store.push("s", refs(&[("a", "2"), ("b", "3")])).unwrap();

        // The whole point: two runs pinned to this snapshot compared the same
        // data, no matter what the working set did afterward.
        let (_, id, cases) = store.resolve(&format!("s@{}", snap.id)).unwrap();
        assert_eq!(id.as_deref(), Some(snap.id.as_str()));
        assert_eq!(cases.len(), 1);
        assert_eq!(cases, snap.cases);

        let (_, id, working) = store.resolve("s").unwrap();
        assert!(id.is_none());
        assert_eq!(working.len(), 2);
    }

    #[test]
    fn snapshots_resolve_by_id_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = SuiteStore::open(dir.path().to_str().unwrap()).unwrap();
        store.push("s", refs(&[("a", "1")])).unwrap();
        let snap = store.snapshot("s", None).unwrap();
        let prefix = &snap.id[..10];
        assert_eq!(
            store.resolve(&format!("s@{prefix}")).unwrap().1.unwrap(),
            snap.id
        );
    }

    #[test]
    fn unknown_suites_and_snapshots_are_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = SuiteStore::open(dir.path().to_str().unwrap()).unwrap();
        assert!(
            store
                .resolve("nope")
                .unwrap_err()
                .to_string()
                .contains("nope")
        );
        store.push("s", refs(&[("a", "1")])).unwrap();
        assert!(
            store
                .resolve("s@snap_missing")
                .unwrap_err()
                .to_string()
                .contains("snap_missing")
        );
    }

    #[test]
    fn diff_separates_adds_removes_and_edits() {
        let from = refs(&[("keep", "1"), ("edit", "before"), ("drop", "1")]);
        let to = refs(&[("keep", "1"), ("edit", "after"), ("new", "1")]);
        let d = diff_cases(&from, &to);

        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].case_id, "new");
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0].case_id, "drop");
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].case_id, "edit");
        assert_eq!(d.unchanged, 1);
    }

    #[test]
    fn suites_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let store = SuiteStore::open(path).unwrap();
        store.push("s", refs(&[("a", "1")])).unwrap();
        let snap = store
            .snapshot("s", Some("before the refactor".into()))
            .unwrap();

        let reopened = SuiteStore::open(path).unwrap();
        let suite = reopened.get("s").unwrap();
        assert_eq!(suite.cases.len(), 1);
        assert_eq!(suite.snapshots.len(), 1);
        assert_eq!(suite.snapshots[0].id, snap.id);
        assert_eq!(
            suite.snapshots[0].note.as_deref(),
            Some("before the refactor")
        );
    }

    #[test]
    fn an_empty_suite_cannot_be_snapshotted() {
        let dir = tempfile::tempdir().unwrap();
        let store = SuiteStore::open(dir.path().to_str().unwrap()).unwrap();
        store.push("empty", Vec::new()).unwrap();
        assert!(store.snapshot("empty", None).is_err());
    }
}
