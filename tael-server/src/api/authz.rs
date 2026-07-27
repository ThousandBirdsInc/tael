//! Request authentication and authorization for every HTTP listener.
//!
//! One layer covers the REST query API, OTLP/HTTP, Prometheus remote-write, and
//! the Datadog trace-agent intake, so there is no listener that can be reached
//! without passing the same check. The gRPC listener uses
//! [`grpc_interceptor`], which resolves principals from the same keystore.
//!
//! The required role is derived from the request rather than declared per
//! route: reads need [`Role::Reader`], anything that mutates state needs
//! [`Role::Writer`], and the key-management surface needs [`Role::Admin`].
//! Deriving it means a route added later is protected by default instead of
//! being accidentally public.

use std::sync::{Arc, RwLock};

use axum::{
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::auth::{AuthMode, KeyStore, Principal, Role};

/// Shared auth state: the mode plus a keystore cache that notices when
/// `tael auth create-key` rewrites the file underneath a running server.
pub struct AuthState {
    mode: AuthMode,
    data_dir: String,
    cache: RwLock<CachedKeys>,
}

struct CachedKeys {
    /// Modification time the cached copy was read at, so an out-of-band
    /// `create-key`/`revoke` takes effect without a restart.
    mtime: Option<std::time::SystemTime>,
    store: Arc<KeyStore>,
}

impl AuthState {
    pub fn new(mode: AuthMode, data_dir: &str) -> anyhow::Result<Self> {
        let store = KeyStore::load(data_dir)?;
        Ok(Self {
            mode,
            data_dir: data_dir.to_string(),
            cache: RwLock::new(CachedKeys {
                mtime: keystore_mtime(data_dir),
                store: Arc::new(store),
            }),
        })
    }

    pub fn mode(&self) -> AuthMode {
        self.mode
    }

    /// The current keystore, re-read when the file changed on disk.
    fn keys(&self) -> Arc<KeyStore> {
        let on_disk = keystore_mtime(&self.data_dir);
        {
            let cache = self.cache.read().expect("auth cache poisoned");
            if cache.mtime == on_disk {
                return Arc::clone(&cache.store);
            }
        }
        // Changed (or first miss): reload. A read failure keeps the cached copy
        // rather than locking every agent out over a transient IO error.
        match KeyStore::load(&self.data_dir) {
            Ok(store) => {
                let mut cache = self.cache.write().expect("auth cache poisoned");
                cache.mtime = on_disk;
                cache.store = Arc::new(store);
                Arc::clone(&cache.store)
            }
            Err(e) => {
                tracing::warn!(error = %e, "reloading keystore failed; using cached keys");
                Arc::clone(&self.cache.read().expect("auth cache poisoned").store)
            }
        }
    }

    /// Resolve a presented key. `None` when it matches no active key.
    pub fn authenticate(&self, presented: &str) -> Option<Principal> {
        self.keys().authenticate(presented)
    }
}

fn keystore_mtime(data_dir: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(KeyStore::path(data_dir))
        .ok()
        .and_then(|m| m.modified().ok())
}

/// Paths reachable without credentials.
///
/// Liveness and readiness must answer for orchestrators and the Docker
/// `HEALTHCHECK`, which have no key; neither reveals telemetry.
fn is_public(path: &str) -> bool {
    matches!(path, "/healthz" | "/readyz")
}

/// The role a request needs, derived from its method and path.
fn required_role(method: &Method, path: &str) -> Role {
    if path.starts_with("/api/v1/auth") {
        return Role::Admin;
    }
    match *method {
        // Reads. HEAD and OPTIONS ride along with GET.
        Method::GET | Method::HEAD | Method::OPTIONS => Role::Reader,
        // Everything else changes state: telemetry ingest (OTLP, remote-write,
        // dd-trace), annotations, eval scores, WAL replication.
        _ => Role::Writer,
    }
}

/// Pull a presented key out of the request.
///
/// Three header spellings are accepted because three ecosystems reach this
/// server: `Authorization: Bearer` (OTLP, REST, the tael CLI), `X-Tael-Api-Key`
/// (a plain header for constrained clients), and `DD-API-KEY` (what dd-trace
/// clients already send, so pointing one at tael needs no extra config).
fn presented_key(req: &Request) -> Option<String> {
    let headers = req.headers();
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let value = value.trim();
        if let Some(token) = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
        {
            return Some(token.trim().to_string());
        }
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    for name in ["x-tael-api-key", "dd-api-key"] {
        if let Some(value) = headers.get(name).and_then(|v| v.to_str().ok())
            && !value.trim().is_empty()
        {
            return Some(value.trim().to_string());
        }
    }
    None
}

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer realm=\"tael\"")],
        axum::Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

fn forbidden(have: Role, need: Role) -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({
            "error": format!(
                "this key has the `{}` role; `{}` is required for this request",
                have.as_str(),
                need.as_str()
            )
        })),
    )
        .into_response()
}

/// The auth middleware. Attaches the resolved [`Principal`] to the request so
/// handlers can scope reads and writes by tenant.
pub async fn require_auth(
    State(state): State<Arc<AuthState>>,
    mut req: Request,
    next: Next,
) -> Response {
    if state.mode() == AuthMode::Off {
        req.extensions_mut().insert(Principal::anonymous());
        return next.run(req).await;
    }

    let path = req.uri().path().to_string();
    if is_public(&path) {
        return next.run(req).await;
    }

    let Some(presented) = presented_key(&req) else {
        return unauthorized(
            "missing API key: send `Authorization: Bearer <key>` \
             (the tael CLI reads TAEL_API_KEY or --api-key)",
        );
    };

    let Some(principal) = state.authenticate(&presented) else {
        return unauthorized("invalid or revoked API key");
    };

    let needed = required_role(req.method(), &path);
    if !principal.role.satisfies(needed) {
        return forbidden(principal.role, needed);
    }

    req.extensions_mut().insert(principal);
    next.run(req).await
}

/// gRPC counterpart for the OTLP ingest listener. Ingest is a write, so every
/// export needs at least [`Role::Writer`].
#[allow(clippy::result_large_err)]
// `tonic::Status` is the interceptor signature tonic requires; boxing it would
// not compile against the trait.
pub fn grpc_interceptor(
    state: Arc<AuthState>,
) -> impl FnMut(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> + Clone {
    move |req: tonic::Request<()>| {
        if state.mode() == AuthMode::Off {
            return Ok(req);
        }
        let presented = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.trim()
                    .strip_prefix("Bearer ")
                    .or_else(|| v.trim().strip_prefix("bearer "))
                    .unwrap_or(v.trim())
                    .to_string()
            })
            .or_else(|| {
                req.metadata()
                    .get("x-tael-api-key")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.trim().to_string())
            });

        let Some(presented) = presented.filter(|p| !p.is_empty()) else {
            return Err(tonic::Status::unauthenticated(
                "missing API key: send `authorization: Bearer <key>` metadata \
                 (OTEL_EXPORTER_OTLP_HEADERS=\"authorization=Bearer <key>\")",
            ));
        };
        let Some(principal) = state.authenticate(&presented) else {
            return Err(tonic::Status::unauthenticated("invalid or revoked API key"));
        };
        if !principal.role.satisfies(Role::Writer) {
            return Err(tonic::Status::permission_denied(format!(
                "this key has the `{}` role; `writer` is required to push telemetry",
                principal.role.as_str()
            )));
        }
        Ok(req)
    }
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::Body, http::Request as HttpRequest, routing::get};
    use tower::ServiceExt;

    use super::*;
    use crate::auth::Role;

    /// A router with the auth layer in front of one read route and one write
    /// route, mirroring the real listener's shape.
    fn app(mode: AuthMode, data_dir: &str) -> Router {
        let state = Arc::new(AuthState::new(mode, data_dir).unwrap());
        Router::new()
            .route("/api/v1/traces", get(|| async { "traces" }))
            .route("/api/v1/blobs", axum::routing::post(|| async { "stored" }))
            .route("/healthz", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state,
                super::require_auth,
            ))
    }

    async fn call(app: Router, method: &str, path: &str, key: Option<&str>) -> StatusCode {
        let mut builder = HttpRequest::builder().method(method).uri(path);
        if let Some(key) = key {
            builder = builder.header("authorization", format!("Bearer {key}"));
        }
        app.oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn auth_off_lets_everything_through() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        assert_eq!(
            call(app(AuthMode::Off, path), "GET", "/api/v1/traces", None).await,
            StatusCode::OK
        );
        assert_eq!(
            call(app(AuthMode::Off, path), "POST", "/api/v1/blobs", None).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn required_auth_rejects_missing_and_bad_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let mut store = KeyStore::default();
        store.create("reader", Role::Reader, "default");
        store.save(path).unwrap();

        assert_eq!(
            call(app(AuthMode::Required, path), "GET", "/api/v1/traces", None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                app(AuthMode::Required, path),
                "GET",
                "/api/v1/traces",
                Some("tael_r_notarealkey")
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn reader_keys_can_read_but_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let mut store = KeyStore::default();
        let (_, reader) = store.create("reader", Role::Reader, "default");
        store.save(path).unwrap();

        assert_eq!(
            call(
                app(AuthMode::Required, path),
                "GET",
                "/api/v1/traces",
                Some(&reader)
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            call(
                app(AuthMode::Required, path),
                "POST",
                "/api/v1/blobs",
                Some(&reader)
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn writer_keys_can_ingest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let mut store = KeyStore::default();
        let (_, writer) = store.create("writer", Role::Writer, "default");
        store.save(path).unwrap();

        assert_eq!(
            call(
                app(AuthMode::Required, path),
                "POST",
                "/api/v1/blobs",
                Some(&writer)
            )
            .await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn health_endpoints_stay_public_under_required_auth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let mut store = KeyStore::default();
        store.create("k", Role::Admin, "default");
        store.save(path).unwrap();

        assert_eq!(
            call(app(AuthMode::Required, path), "GET", "/healthz", None).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn keys_created_after_startup_are_picked_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let mut store = KeyStore::default();
        store.create("bootstrap", Role::Admin, "default");
        store.save(path).unwrap();

        let state = Arc::new(AuthState::new(AuthMode::Required, path).unwrap());
        // A second key minted out-of-band, as `tael auth create-key` does
        // against a running server.
        let mut store = KeyStore::load(path).unwrap();
        let (_, late) = store.create("late", Role::Reader, "default");
        // mtime has 1s granularity on some filesystems; make the change visible.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store.save(path).unwrap();

        assert!(
            state.authenticate(&late).is_some(),
            "keystore should be reloaded when the file changes"
        );
    }

    #[tokio::test]
    async fn dd_api_key_header_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let mut store = KeyStore::default();
        let (_, writer) = store.create("dd", Role::Writer, "default");
        store.save(path).unwrap();

        let response = app(AuthMode::Required, path)
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/blobs")
                    .header("dd-api-key", writer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
