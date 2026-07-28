//! Tenant scoping.
//!
//! **Two levels, chosen explicitly.**
//!
//! `TAEL_MULTI_TENANT=1` is *authorization*: every record written by an
//! authenticated principal is stamped with that principal's tenant (the
//! server's stamp overrides anything the client sent — see [`stamp`]), and
//! reads are filtered to the reader's tenant. The API will not hand tenant
//! A's traces to tenant B, but all tenants still share one hot tier, one cold
//! tier, and one text index, so anything that bypasses the query layer sees
//! everything. The SQL escape hatch is exactly such a bypass, which is why it
//! is refused for non-admin principals when tenancy is on rather than being
//! silently unscoped.
//!
//! `TAEL_TENANT_ISOLATION=1` (which implies the above) is *isolation*: the
//! tenant becomes the top-level shard key of the storage layout itself and
//! each tenant gets its own complete engine — see
//! [`crate::storage::TenantShardedStore`]. The one deliberately shared piece
//! is the content-addressed payload blob store (cross-tenant dedup, keys
//! reachable only by knowing the content's hash).
//!
//! Saying which of the two you have matters: a deployment that believes it has
//! isolation and only has authorization will put data somewhere it shouldn't.

use crate::auth::{Principal, Role};

/// Attribute stamped on every record to record its owning tenant.
///
/// A reserved `tael.` name so it cannot collide with an application attribute,
/// and stripping it on read would cost a copy of every record for no benefit —
/// a caller seeing its own tenant back is harmless.
pub const TENANT_ATTRIBUTE: &str = "tael.tenant";

/// The default tenant for single-tenant deployments, which is every deployment
/// that never turns tenancy on.
pub const DEFAULT_TENANT: &str = "default";

/// The tenant that owns a record, from its attributes.
///
/// Records written before tenancy was enabled carry no tenant attribute; they
/// belong to the default tenant rather than to nobody, so enabling tenancy on
/// an existing server does not make its history invisible.
pub fn owner_of(attributes: &std::collections::HashMap<String, String>) -> &str {
    attributes
        .get(TENANT_ATTRIBUTE)
        .map(String::as_str)
        .unwrap_or(DEFAULT_TENANT)
}

/// Whether a principal may read across tenants.
///
/// Admin keys can, because operating a server means being able to see what is
/// on it. Every other role is confined to its own tenant.
pub fn may_read_all_tenants(principal: &Principal) -> bool {
    principal.role == Role::Admin
}

/// The tenant a read should be scoped to, or `None` for unscoped.
///
/// Unscoped happens in two cases that look the same to a handler but are very
/// different: tenancy is off (there is one tenant, so scoping is a no-op), or
/// the caller is an admin (allowed to see everything).
pub fn read_scope(enabled: bool, principal: Option<&Principal>) -> Option<String> {
    if !enabled {
        return None;
    }
    match principal {
        Some(p) if may_read_all_tenants(p) => None,
        Some(p) => Some(p.tenant.clone()),
        // No principal with tenancy on means auth is misconfigured. Scoping to
        // a tenant that cannot exist fails closed instead of leaking.
        None => Some(String::new()),
    }
}

/// The tenant to stamp on a write.
pub fn write_tenant(enabled: bool, principal: Option<&Principal>) -> String {
    if !enabled {
        return DEFAULT_TENANT.to_string();
    }
    principal
        .map(|p| p.tenant.clone())
        .unwrap_or_else(|| DEFAULT_TENANT.to_string())
}

/// Stamp the writer's tenant onto a batch of attribute maps.
///
/// With tenancy off this is a no-op — single-tenant records carry no tenant
/// attribute, exactly as before. With tenancy on the server's stamp
/// **overwrites** anything the client sent: the attribute is an authorization
/// boundary, and honoring a client-supplied value would let any writer place
/// records in (and read them back from) another tenant by forging one field.
pub fn stamp<'a>(
    enabled: bool,
    principal: Option<&Principal>,
    attribute_maps: impl Iterator<Item = &'a mut std::collections::HashMap<String, String>>,
) {
    if !enabled {
        return;
    }
    stamp_resolved(Some(&write_tenant(enabled, principal)), attribute_maps);
}

/// [`stamp`] with the tenant already resolved. `None` means tenancy is off
/// (no-op); handlers that resolve the tenant once and hand it to an ingest
/// helper use this form.
pub fn stamp_resolved<'a>(
    tenant: Option<&str>,
    attribute_maps: impl Iterator<Item = &'a mut std::collections::HashMap<String, String>>,
) {
    let Some(tenant) = tenant else { return };
    for attributes in attribute_maps {
        attributes.insert(TENANT_ATTRIBUTE.to_string(), tenant.to_string());
    }
}

/// Whether a principal may use the SQL escape hatch.
///
/// SQL runs against tables the query layer cannot filter per-row without
/// rewriting arbitrary user queries, so with tenancy on it is admin-only.
/// Refusing is the honest option: silently returning every tenant's rows to a
/// `SELECT *` would be a data leak wearing a feature's clothes.
pub fn may_use_sql(enabled: bool, principal: Option<&Principal>) -> bool {
    if !enabled {
        return true;
    }
    principal.is_some_and(may_read_all_tenants)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(role: Role, tenant: &str) -> Principal {
        Principal {
            key_id: "k".into(),
            name: "n".into(),
            role,
            tenant: tenant.into(),
        }
    }

    #[test]
    fn records_without_a_tenant_belong_to_the_default_one() {
        // Enabling tenancy on an existing server must not make its history
        // invisible to everyone.
        let mut attrs = std::collections::HashMap::new();
        assert_eq!(owner_of(&attrs), DEFAULT_TENANT);
        attrs.insert(TENANT_ATTRIBUTE.to_string(), "team-a".to_string());
        assert_eq!(owner_of(&attrs), "team-a");
    }

    #[test]
    fn tenancy_off_scopes_nothing() {
        // The single-tenant path must stay exactly as it was.
        assert_eq!(read_scope(false, None), None);
        assert_eq!(
            read_scope(false, Some(&principal(Role::Reader, "team-a"))),
            None
        );
        assert_eq!(write_tenant(false, None), DEFAULT_TENANT);
        assert!(may_use_sql(false, None));
    }

    #[test]
    fn readers_and_writers_are_confined_to_their_tenant() {
        for role in [Role::Reader, Role::Writer] {
            assert_eq!(
                read_scope(true, Some(&principal(role, "team-a"))),
                Some("team-a".to_string())
            );
        }
    }

    #[test]
    fn admins_read_across_tenants() {
        // Operating a server means being able to see what is on it.
        assert_eq!(read_scope(true, Some(&principal(Role::Admin, "ops"))), None);
        assert!(may_read_all_tenants(&principal(Role::Admin, "ops")));
        assert!(!may_read_all_tenants(&principal(Role::Writer, "ops")));
    }

    #[test]
    fn a_missing_principal_with_tenancy_on_fails_closed() {
        // Auth misconfigured: scope to a tenant nothing can match rather than
        // returning every tenant's data.
        assert_eq!(read_scope(true, None), Some(String::new()));
    }

    #[test]
    fn writes_are_stamped_with_the_writers_tenant() {
        assert_eq!(
            write_tenant(true, Some(&principal(Role::Writer, "team-b"))),
            "team-b"
        );
        assert_eq!(write_tenant(true, None), DEFAULT_TENANT);
    }

    #[test]
    fn a_scoped_query_returns_only_that_tenants_records() {
        // The load-bearing assertion: the filter has to work at the storage
        // layer, not just compute the right scope string.
        use crate::storage::Store;
        use crate::storage::models::{Span, SpanKind, SpanStatus, TraceQuery};
        use crate::storage::testing::TestBackend;

        let engine = TestBackend::new();
        let now = chrono::Utc::now();
        let span = |id: &str, tenant: Option<&str>| {
            let mut attributes = std::collections::HashMap::new();
            if let Some(t) = tenant {
                attributes.insert(TENANT_ATTRIBUTE.to_string(), t.to_string());
            }
            Span {
                trace_id: format!("t{id}"),
                span_id: format!("s{id}"),
                parent_span_id: None,
                service: "api".into(),
                operation: "op".into(),
                start_time: now,
                end_time: now,
                duration_ms: 1.0,
                status: SpanStatus::Ok,
                attributes,
                events: vec![],
                kind: SpanKind::Server,
                llm: None,
            }
        };
        engine
            .backend
            .insert_spans(&[
                span("a1", Some("team-a")),
                span("a2", Some("team-a")),
                span("b1", Some("team-b")),
                span("legacy", None),
            ])
            .unwrap();

        let scoped = |tenant: Option<&str>| {
            engine
                .backend
                .query_traces(&TraceQuery {
                    limit: Some(100),
                    tenant: tenant.map(str::to_string),
                    ..Default::default()
                })
                .unwrap()
        };

        let a = scoped(Some("team-a"));
        assert_eq!(a.len(), 2);
        assert!(a.iter().all(|s| s.trace_id.starts_with("ta")));

        let b = scoped(Some("team-b"));
        assert_eq!(b.len(), 1);

        // Pre-tenancy records read as the default tenant, not as nobody's.
        assert_eq!(scoped(Some(DEFAULT_TENANT)).len(), 1);

        // Unscoped (admin, or tenancy off) sees everything.
        assert_eq!(scoped(None).len(), 4);

        // A tenant with no records gets nothing, which is also what a
        // fail-closed empty scope produces.
        assert!(scoped(Some("")).is_empty());
    }

    #[test]
    fn sql_is_admin_only_under_tenancy() {
        // It cannot be filtered per row without rewriting arbitrary queries,
        // so it is refused rather than silently unscoped.
        assert!(!may_use_sql(true, None));
        assert!(!may_use_sql(true, Some(&principal(Role::Reader, "a"))));
        assert!(!may_use_sql(true, Some(&principal(Role::Writer, "a"))));
        assert!(may_use_sql(true, Some(&principal(Role::Admin, "ops"))));
    }
}
