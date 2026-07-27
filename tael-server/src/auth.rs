//! API-key authentication (`DESIGN.md` §Agent Auth Model).
//!
//! Agents are first-class principals here: a key *is* an identity, and its role
//! is baked into the key's own prefix (`tael_r_`, `tael_w_`, `tael_a_`) so an
//! agent holding one can tell what it may do without a round trip.
//!
//! Two properties drive the design:
//!
//! * **Zero friction on loopback.** The historical behavior — bind
//!   `127.0.0.1`, no ceremony — is preserved exactly. Auth only turns itself on
//!   when the server is reachable from off-box.
//! * **Fail closed off-box.** Binding a non-loopback address with no keys and
//!   no explicit opt-out is refused at startup rather than silently publishing
//!   an unauthenticated telemetry store. See [`AuthMode::resolve`].
//!
//! Keys are stored as salted SHA-256 digests in `<data_dir>/api_keys.json`; the
//! plaintext is shown exactly once, at creation, and is unrecoverable after.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Filename of the keystore inside the data directory.
const KEYSTORE_FILE: &str = "api_keys.json";

/// What a key is allowed to do. Ordered: each role subsumes the ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Query traces, logs, metrics, summaries, anomalies, topology.
    Reader,
    /// Everything a reader can do, plus pushing telemetry (OTLP, remote-write,
    /// dd-trace) and writing annotations (comments, issues, eval scores).
    Writer,
    /// Everything a writer can do, plus server administration.
    Admin,
}

impl Role {
    /// The key prefix that encodes this role.
    pub fn prefix(self) -> &'static str {
        match self {
            Role::Reader => "tael_r_",
            Role::Writer => "tael_w_",
            Role::Admin => "tael_a_",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Reader => "reader",
            Role::Writer => "writer",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "reader" | "read" | "r" => Ok(Role::Reader),
            "writer" | "write" | "w" => Ok(Role::Writer),
            "admin" | "a" => Ok(Role::Admin),
            other => bail!("unknown role `{other}` (expected reader, writer, or admin)"),
        }
    }

    /// Whether a principal with this role satisfies a requirement of `needed`.
    pub fn satisfies(self, needed: Role) -> bool {
        self >= needed
    }
}

/// Whether the server enforces authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// No credentials checked — reachability is authorization. Only sound when
    /// every listener is bound to loopback.
    #[default]
    Off,
    /// Every non-public route requires a valid key.
    Required,
}

impl AuthMode {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "off" | "none" | "disabled" | "false" | "0" => Ok(AuthMode::Off),
            "required" | "require" | "on" | "true" | "1" => Ok(AuthMode::Required),
            other => bail!("unknown auth mode `{other}` (expected off or required)"),
        }
    }

    /// Decide the effective mode for a set of listen addresses.
    ///
    /// An explicit setting always wins. With nothing set, loopback-only stays
    /// `Off` (the zero-friction local default) and any off-box listener becomes
    /// `Required` — so a deployment that exposes tael to a network gets auth by
    /// default instead of by remembering to ask for it.
    pub fn resolve(explicit: Option<AuthMode>, listeners: &[&str]) -> Self {
        if let Some(mode) = explicit {
            return mode;
        }
        if listeners.iter().any(|addr| !is_loopback_addr(addr)) {
            AuthMode::Required
        } else {
            AuthMode::Off
        }
    }
}

/// Whether a `host:port` listen address is loopback-only.
///
/// `0.0.0.0` and `::` are treated as off-box because they accept traffic on
/// every interface, which is the case that matters for the fail-closed rule.
/// An unparseable or name-based host is treated as off-box too: guessing
/// "probably local" is the failure mode we're trying to avoid.
fn is_loopback_addr(addr: &str) -> bool {
    let host = match addr.rsplit_once(':') {
        Some((h, _)) => h,
        None => addr,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        // Bare hostnames: only the well-known local names are trusted.
        Err(_) => matches!(host, "localhost" | ""),
    }
}

/// A stored key record. The plaintext key never appears here — only its digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    /// Stable public identifier, safe to log and to pass to `auth revoke`.
    pub id: String,
    /// Operator-supplied label, e.g. `claude-code-prod`.
    pub name: String,
    pub role: Role,
    /// Hex SHA-256 of `salt || plaintext`.
    hash: String,
    /// Per-key random salt, hex encoded.
    salt: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Set when revoked; a revoked key never authenticates again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Tenant this key reads and writes within. Single-tenant deployments
    /// leave this at `default`.
    #[serde(default = "default_tenant")]
    pub tenant: String,
}

fn default_tenant() -> String {
    "default".to_string()
}

impl ApiKeyRecord {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// An authenticated caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub key_id: String,
    pub name: String,
    pub role: Role,
    pub tenant: String,
}

impl Principal {
    /// The implicit principal when auth is off: full rights, single tenant.
    /// Named so it is obvious in logs that no credential was presented.
    pub fn anonymous() -> Self {
        Self {
            key_id: "anonymous".into(),
            name: "anonymous".into(),
            role: Role::Admin,
            tenant: default_tenant(),
        }
    }
}

/// The on-disk key set.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KeyStore {
    #[serde(default)]
    pub keys: Vec<ApiKeyRecord>,
}

impl KeyStore {
    /// Path of the keystore for a data directory.
    pub fn path(data_dir: &str) -> PathBuf {
        Path::new(data_dir).join(KEYSTORE_FILE)
    }

    /// Load the keystore, treating "no file yet" as an empty set.
    pub fn load(data_dir: &str) -> Result<Self> {
        let path = Self::path(data_dir);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing keystore {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading keystore {}", path.display())),
        }
    }

    /// Write the keystore back, owner-readable only.
    pub fn save(&self, data_dir: &str) -> Result<()> {
        let path = Self::path(data_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let json = serde_json::to_vec_pretty(self)?;
        // Write-then-rename so a crash mid-write can't truncate the keystore
        // and lock every agent out.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).with_context(|| format!("writing {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &path).with_context(|| format!("installing {}", path.display()))?;
        Ok(())
    }

    /// Whether any key can still authenticate.
    pub fn has_active_keys(&self) -> bool {
        self.keys.iter().any(ApiKeyRecord::is_active)
    }

    /// Mint a new key. Returns the record and the plaintext, which is the only
    /// time the plaintext exists — it is not recoverable from the keystore.
    pub fn create(&mut self, name: &str, role: Role, tenant: &str) -> (ApiKeyRecord, String) {
        let secret = random_hex(24);
        let salt = random_hex(16);
        let plaintext = format!("{}{}", role.prefix(), secret);
        let record = ApiKeyRecord {
            id: format!("key_{}", random_hex(6)),
            name: name.to_string(),
            role,
            hash: digest(&salt, &plaintext),
            salt,
            created_at: chrono::Utc::now(),
            revoked_at: None,
            tenant: tenant.to_string(),
        };
        self.keys.push(record.clone());
        (record, plaintext)
    }

    /// Revoke by key id. Returns false when the id is unknown or already
    /// revoked.
    pub fn revoke(&mut self, key_id: &str) -> bool {
        match self
            .keys
            .iter_mut()
            .find(|k| k.id == key_id && k.revoked_at.is_none())
        {
            Some(k) => {
                k.revoked_at = Some(chrono::Utc::now());
                true
            }
            None => false,
        }
    }

    /// Resolve a presented key to its principal, or `None` when it matches no
    /// active key.
    pub fn authenticate(&self, presented: &str) -> Option<Principal> {
        let presented = presented.trim();
        if presented.is_empty() {
            return None;
        }
        // Every active key is checked even after a match so that a wrong key
        // costs the same time as a right one.
        let mut found: Option<&ApiKeyRecord> = None;
        for key in self.keys.iter().filter(|k| k.is_active()) {
            let candidate = digest(&key.salt, presented);
            if constant_time_eq(candidate.as_bytes(), key.hash.as_bytes()) {
                found = Some(key);
            }
        }
        found.map(|key| Principal {
            key_id: key.id.clone(),
            name: key.name.clone(),
            role: key.role,
            tenant: key.tenant.clone(),
        })
    }
}

fn digest(salt: &str, plaintext: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(plaintext.as_bytes());
    hex::encode(hasher.finalize())
}

/// Compare two equal-length byte strings without short-circuiting on the first
/// differing byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_subsume_lower_roles() {
        assert!(Role::Admin.satisfies(Role::Reader));
        assert!(Role::Admin.satisfies(Role::Writer));
        assert!(Role::Writer.satisfies(Role::Reader));
        assert!(!Role::Reader.satisfies(Role::Writer));
        assert!(!Role::Writer.satisfies(Role::Admin));
    }

    #[test]
    fn key_prefix_encodes_its_role() {
        let mut store = KeyStore::default();
        let (_, reader) = store.create("r", Role::Reader, "default");
        let (_, writer) = store.create("w", Role::Writer, "default");
        let (_, admin) = store.create("a", Role::Admin, "default");
        assert!(reader.starts_with("tael_r_"));
        assert!(writer.starts_with("tael_w_"));
        assert!(admin.starts_with("tael_a_"));
    }

    #[test]
    fn authenticates_only_the_matching_plaintext() {
        let mut store = KeyStore::default();
        let (record, plaintext) = store.create("claude-code", Role::Writer, "default");

        let principal = store.authenticate(&plaintext).expect("key should resolve");
        assert_eq!(principal.key_id, record.id);
        assert_eq!(principal.role, Role::Writer);
        assert_eq!(principal.name, "claude-code");

        assert!(store.authenticate("tael_w_deadbeef").is_none());
        assert!(store.authenticate("").is_none());
    }

    #[test]
    fn revoked_keys_stop_authenticating() {
        let mut store = KeyStore::default();
        let (record, plaintext) = store.create("temp", Role::Reader, "default");
        assert!(store.authenticate(&plaintext).is_some());

        assert!(store.revoke(&record.id));
        assert!(store.authenticate(&plaintext).is_none());
        // Revoking twice is a no-op, not an error the caller must special-case.
        assert!(!store.revoke(&record.id));
        assert!(!store.has_active_keys());
    }

    #[test]
    fn plaintext_is_not_recoverable_from_the_keystore() {
        let mut store = KeyStore::default();
        let (_, plaintext) = store.create("secret", Role::Admin, "default");
        let serialized = serde_json::to_string(&store).unwrap();
        assert!(
            !serialized.contains(&plaintext),
            "keystore must not persist the plaintext key"
        );
    }

    #[test]
    fn keystore_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let mut store = KeyStore::default();
        let (record, plaintext) = store.create("agent", Role::Reader, "team-a");
        store.save(path).unwrap();

        let loaded = KeyStore::load(path).unwrap();
        let principal = loaded.authenticate(&plaintext).unwrap();
        assert_eq!(principal.key_id, record.id);
        assert_eq!(principal.tenant, "team-a");
    }

    #[test]
    fn missing_keystore_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::load(dir.path().to_str().unwrap()).unwrap();
        assert!(store.keys.is_empty());
        assert!(!store.has_active_keys());
    }

    #[test]
    fn loopback_listeners_default_to_no_auth() {
        let mode = AuthMode::resolve(None, &["127.0.0.1:7701", "127.0.0.1:4317", "[::1]:4318"]);
        assert_eq!(mode, AuthMode::Off);
        assert_eq!(AuthMode::resolve(None, &["localhost:7701"]), AuthMode::Off);
    }

    #[test]
    fn off_box_listeners_default_to_required_auth() {
        // The Docker case: binding every interface must not publish an
        // unauthenticated server by default.
        assert_eq!(
            AuthMode::resolve(None, &["127.0.0.1:7701", "0.0.0.0:4317"]),
            AuthMode::Required
        );
        assert_eq!(
            AuthMode::resolve(None, &["192.168.1.5:7701"]),
            AuthMode::Required
        );
        assert_eq!(AuthMode::resolve(None, &["[::]:7701"]), AuthMode::Required);
        // An unrecognized host is treated as off-box rather than assumed local.
        assert_eq!(
            AuthMode::resolve(None, &["telemetry.internal:7701"]),
            AuthMode::Required
        );
    }

    #[test]
    fn explicit_mode_overrides_the_listener_heuristic() {
        assert_eq!(
            AuthMode::resolve(Some(AuthMode::Off), &["0.0.0.0:7701"]),
            AuthMode::Off
        );
        assert_eq!(
            AuthMode::resolve(Some(AuthMode::Required), &["127.0.0.1:7701"]),
            AuthMode::Required
        );
    }

    #[test]
    fn auth_mode_and_role_parse_their_aliases() {
        assert_eq!(AuthMode::parse("required").unwrap(), AuthMode::Required);
        assert_eq!(AuthMode::parse("OFF").unwrap(), AuthMode::Off);
        assert!(AuthMode::parse("maybe").is_err());
        assert_eq!(Role::parse("Admin").unwrap(), Role::Admin);
        assert!(Role::parse("superuser").is_err());
    }
}
