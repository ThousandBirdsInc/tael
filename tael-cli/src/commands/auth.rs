//! `tael auth` — API key management.
//!
//! These commands operate directly on the keystore file in the data directory
//! rather than through the REST API, for the same reason `htpasswd` does: the
//! first key has to be mintable before any key exists to authenticate the
//! request that would mint it. Being local also means key management works on
//! a server that is refusing to start.

use anyhow::{Context, Result};
use tael_server::auth::{KeyStore, Role};

use crate::OutputFormat;
use crate::output::print_json;

/// Mint a key and print it. The plaintext is shown exactly once — the keystore
/// only ever holds its salted digest — so the output says so plainly.
pub fn create_key(
    format: &OutputFormat,
    data_dir: &str,
    name: &str,
    role: &str,
    tenant: &str,
) -> Result<()> {
    let role = Role::parse(role)?;
    let mut store = KeyStore::load(data_dir)?;
    if store.keys.iter().any(|k| k.is_active() && k.name == name) {
        anyhow::bail!(
            "an active key named `{name}` already exists; \
             revoke it first or choose another name"
        );
    }
    let (record, plaintext) = store.create(name, role, tenant);
    store
        .save(data_dir)
        .with_context(|| format!("saving keystore in {data_dir}"))?;

    let payload = serde_json::json!({
        "id": record.id,
        "name": record.name,
        "role": role.as_str(),
        "tenant": record.tenant,
        "created_at": record.created_at,
        "key": plaintext,
        "note": "store this key now — it is not recoverable from the server",
    });

    match format {
        OutputFormat::Json => print_json(&payload),
        OutputFormat::Table => {
            println!("Created API key `{}` ({})", record.name, role.as_str());
            println!("  id      {}", record.id);
            println!("  tenant  {}", record.tenant);
            println!("  key     {plaintext}");
            println!();
            println!("Store this key now — it is not recoverable from the server.");
            println!("Use it with:");
            println!("  export TAEL_API_KEY={plaintext}");
        }
    }
    Ok(())
}

/// List keys. Never prints key material — only the metadata needed to decide
/// what to revoke.
pub fn list_keys(format: &OutputFormat, data_dir: &str) -> Result<()> {
    let store = KeyStore::load(data_dir)?;
    let keys: Vec<_> = store
        .keys
        .iter()
        .map(|k| {
            serde_json::json!({
                "id": k.id,
                "name": k.name,
                "role": k.role.as_str(),
                "tenant": k.tenant,
                "created_at": k.created_at,
                "revoked_at": k.revoked_at,
                "active": k.is_active(),
            })
        })
        .collect();

    match format {
        OutputFormat::Json => print_json(&serde_json::json!({
            "keystore": KeyStore::path(data_dir).display().to_string(),
            "count": keys.len(),
            "keys": keys,
        })),
        OutputFormat::Table => {
            if keys.is_empty() {
                println!("No API keys in {}", KeyStore::path(data_dir).display());
                return Ok(());
            }
            let mut table = comfy_table::Table::new();
            table.set_header(vec!["ID", "NAME", "ROLE", "TENANT", "CREATED", "STATUS"]);
            for k in &store.keys {
                table.add_row(vec![
                    k.id.clone(),
                    k.name.clone(),
                    k.role.as_str().to_string(),
                    k.tenant.clone(),
                    k.created_at.format("%Y-%m-%d %H:%M").to_string(),
                    if k.is_active() {
                        "active".into()
                    } else {
                        "revoked".to_string()
                    },
                ]);
            }
            println!("{table}");
        }
    }
    Ok(())
}

/// Revoke a key by id. Takes effect on a running server without a restart —
/// the auth layer notices the keystore's new mtime.
pub fn revoke_key(format: &OutputFormat, data_dir: &str, key_id: &str) -> Result<()> {
    let mut store = KeyStore::load(data_dir)?;
    if !store.revoke(key_id) {
        anyhow::bail!("no active key with id `{key_id}` (see `tael auth list`)");
    }
    store.save(data_dir)?;

    let payload = serde_json::json!({ "revoked": key_id, "status": "revoked" });
    match format {
        OutputFormat::Json => print_json(&payload),
        OutputFormat::Table => println!("Revoked key {key_id}"),
    }
    Ok(())
}
