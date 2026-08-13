//! Authentication middleware: `peacock_session` cookie → JWT → `CallerContext`.
//!
//! Extracts the `peacock_session` HttpOnly cookie, verifies the HS256 JWT that
//! `POST /api/auth/login` issued, and injects a [`CallerContext`] into request
//! extensions. Handlers obtain it via the [`CallerContext`] extractor or enforce
//! a minimum role with [`require_role!`].
//!
//! ```ignore
//! use peacock_api::middleware::auth::{CallerContext, Role, require_role};
//!
//! async fn handler(caller: CallerContext) -> ApiResult<Json<Value>> {
//!     require_role!(caller, Role::Manager);
//!     Ok(Json(json!({ "you": caller.user_id })))
//! }
//! ```
//!
//! Public paths (`/health`, `/health/ready`, `/api/auth/login`, `/api/auth/logout`)
//! and `OPTIONS` preflight bypass authentication. When a `peacock_session`
//! cookie is present it is verified; an invalid or expired JWT receives
//! `401 Unauthorized` as `application/problem+json` (via [`crate::error::ApiError`]),
//! so the error layer can enrich it with `instance` and `request_id`.
//! When no cookie is present the request proceeds without a [`CallerContext`];
//! handlers that require authentication take `CallerContext` as an extractor
//! and receive 401 there, so anonymous `GET /api/does-not-exist` stays 404
//! rather than 401 and existing routing tests keep passing.

use axum::extract::{FromRef, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AppState;

/// Cookie name set by `POST /api/auth/login`.
///
/// `HttpOnly; Secure; SameSite=Lax` — see `docs/DEVELOPER_PLATFORM_PLAN.md §3.2`.
pub const PEACOCK_SESSION_COOKIE: &str = "peacock_session";

/// Header fallback for service-to-service calls (API keys over Bearer).
const AUTHORIZATION: HeaderName = HeaderName::from_static("authorization");

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

/// RBAC roles, ordered low → high.
///
/// `owner > manager > cashier > waiter` per `DEVELOPER_PLATFORM_PLAN.md §3.2`.
/// `Dev` is alias for `Owner` in hierarchy (highest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Waiter,
    Cashier,
    Manager,
    Owner,
    #[serde(rename = "dev")]
    Dev,
}

impl Role {
    /// Numeric level for hierarchy checks.
    pub fn level(self) -> u8 {
        match self {
            Self::Waiter => 0,
            Self::Cashier => 1,
            Self::Manager => 2,
            Self::Owner => 3,
            Self::Dev => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiter => "waiter",
            Self::Cashier => "cashier",
            Self::Manager => "manager",
            Self::Owner => "owner",
            Self::Dev => "dev",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "waiter" => Some(Self::Waiter),
            "cashier" => Some(Self::Cashier),
            "manager" => Some(Self::Manager),
            "owner" => Some(Self::Owner),
            "dev" | "developer" | "admin" => Some(Self::Dev),
            _ => None,
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| format!("unknown role {s:?}"))
    }
}

// ---------------------------------------------------------------------------
// JWT Claims
// ---------------------------------------------------------------------------

/// Claims carried in the `peacock_session` JWT.
///
/// Issued by `POST /api/auth/login`, verified here. `exp` is validated by
/// `jsonwebtoken`; `iat` is informational. `restaurant`/`branch` bind the
/// session to an outlet — handlers must not trust `X-Restaurant` when a
/// session is present (W4_SECURITY §6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — `users.id` (UUID) or email for legacy seeds.
    pub sub: String,
    pub role: String,
    #[serde(default)]
    pub restaurant: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    pub exp: usize,
    #[serde(default)]
    pub iat: Option<usize>,
}

// ---------------------------------------------------------------------------
// CallerContext
// ---------------------------------------------------------------------------

/// Authenticated caller for one request.
///
/// Cloned into handlers and into log spans. Cheap: all fields are `String`/
/// newtypes, no `Arc` needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerContext {
    /// `users.id` or `sub` from the JWT.
    pub user_id: String,
    /// Raw email if present in claims.
    pub email: Option<String>,
    pub role: Role,
    pub restaurant: Option<peacock_core::ids::RestaurantName>,
    pub branch: Option<peacock_core::ids::BranchName>,
    /// Expiry as unix seconds, for logging/debugging.
    pub exp: usize,
}

impl CallerContext {
    pub fn from_claims(claims: Claims) -> Result<Self, ApiError> {
        let role = Role::parse(&claims.role).ok_or_else(|| {
            ApiError::unauthorized(format!("unknown role in session token: {:?}", claims.role))
        })?;

        let restaurant = claims
            .restaurant
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| peacock_core::ids::RestaurantName::new(s.trim().to_string()));

        let branch = claims
            .branch
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| peacock_core::ids::BranchName::new(s.trim().to_string()));

        Ok(Self {
            user_id: claims.sub,
            email: claims.email,
            role,
            restaurant,
            branch,
            exp: claims.exp,
        })
    }

    pub fn has_role(&self, required: Role) -> bool {
        self.role.level() >= required.level()
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn restaurant(&self) -> Option<&peacock_core::ids::RestaurantName> {
        self.restaurant.as_ref()
    }

    pub fn branch(&self) -> Option<&peacock_core::ids::BranchName> {
        self.branch.as_ref()
    }
}

// ---------------------------------------------------------------------------
// Cookie / header extraction
// ---------------------------------------------------------------------------

/// Extracts the value of `peacock_session` from the `Cookie` header(s).
///
/// The `Cookie` header may appear once with `;`-separated pairs, or multiple
/// times (proxies). We scan all values and all pairs.
pub fn extract_peacock_session_cookie(headers: &HeaderMap) -> Option<String> {
    // Collect all Cookie header values, split on ';', trim, match prefix.
    let prefix = format!("{PEACOCK_SESSION_COOKIE}=");

    for value in headers.get_all(axum::http::header::COOKIE).iter() {
        let Ok(raw) = value.to_str() else {
            continue;
        };
        for part in raw.split(';') {
            let trimmed = part.trim();
            if let Some(after) = trimmed.strip_prefix(&prefix) {
                let token = after.trim().trim_matches('"').trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

/// Also accepts `Authorization: Bearer <jwt>` as fallback for programmatic clients.
fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let trimmed = value.trim();
    let after = trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))?;
    let token = after.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Unified extractor: cookie wins, Bearer is fallback.
pub fn extract_token(headers: &HeaderMap) -> Option<String> {
    extract_peacock_session_cookie(headers).or_else(|| extract_bearer_token(headers))
}

// ---------------------------------------------------------------------------
// JWT verification
// ---------------------------------------------------------------------------

/// Verifies `token` as HS256 JWT with `secret`.
///
/// `exp` is enforced; `iat`/`nbf` are not required. Algorithm is pinned to
/// HS256 so a `none` or RS256 token cannot be smuggled in.
pub fn verify_jwt(token: &str, secret: &str) -> Result<Claims, ApiError> {
    use jsonwebtoken::{Algorithm, DecodingKey, Validation};

    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    validation.validate_nbf = false;
    // `aud`/`iss` are not used by `peacock_session`; keep defaults (no validation).
    validation.validate_aud = false;

    let key = DecodingKey::from_secret(secret.as_bytes());
    let data = jsonwebtoken::decode::<Claims>(token, &key, &validation).map_err(|e| {
        // Do not leak `e` detail to the client — log it and return 401.
        tracing::debug!(error = %e, "jwt verification failed");
        ApiError::unauthorized("invalid or expired session")
    })?;
    Ok(data.claims)
}

/// Convenience for tests and login handlers: mint a token (HS256).
pub fn mint_jwt(claims: &Claims, secret: &str) -> Result<String, jsonwebtoken::errors::Error> {
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    let header = Header::new(Algorithm::HS256);
    let key = EncodingKey::from_secret(secret.as_bytes());
    jsonwebtoken::encode(&header, claims, &key)
}

// ---------------------------------------------------------------------------
// Public-path bypass
// ---------------------------------------------------------------------------

fn is_public_path(path: &str) -> bool {
    // Health probes must be reachable without a session — the load balancer
    // and the readiness check run before any login exists.
    if path == "/health" || path == "/health/ready" {
        return true;
    }
    // Auth endpoints themselves (including demo PIN login).
    if path == "/api/auth/login" || path == "/api/auth/logout" || path == "/api/auth/pin-login" {
        return true;
    }
    // Let CORS preflight through without a session; the browser will then
    // send the real request with the cookie + Authorization.
    false
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Middleware: verify `peacock_session` and inject [`CallerContext`].
///
/// Runs *inside* the error layer so a 401 produced here still gets
/// `request_id` + `instance` + `problem+json` enrichment, and *outside* the
/// CORS layer so real errors still carry `Access-Control-Allow-Origin`.
///
/// Bypassed for:
///
/// * `OPTIONS` requests (CORS preflight)
/// * `is_public_path` — `/health`, `/health/ready`, `/api/auth/login`, `/api/auth/logout`
/// * Requests without a `peacock_session` cookie — the handler's `CallerContext`
///   extractor is the gatekeeper, so anonymous routing (404/405) stays honest.
pub async fn authenticate(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    // Preflight never carries the HttpOnly cookie; rejecting it with 401
    // would break every cross-origin read from the Vercel frontend.
    if request.method() == axum::http::Method::OPTIONS {
        return next.run(request).await;
    }

    let path = request.uri().path().to_string();

    if is_public_path(&path) {
        return next.run(request).await;
    }

    let headers = request.headers().clone();

    let Some(token) = extract_token(&headers) else {
        return next.run(request).await;
    };

    let secret = state.config().jwt_secret.clone();

    // Warn once if the process is running with the dev default — operator action needed.
    if secret == "dev-jwt-secret-change-me-in-production" {
        tracing::warn!(
            "PEACOCK_JWT_SECRET is not set; using insecure dev default. \
             Set PEACOCK_JWT_SECRET to a high-entropy value (e.g. `openssl rand -hex 32`) \
             before exposing this build beyond localhost."
        );
    }

    let claims = match verify_jwt(&token, &secret) {
        Ok(claims) => claims,
        Err(api_err) => return api_err.into_response(),
    };

    let ctx = match CallerContext::from_claims(claims) {
        Ok(ctx) => ctx,
        Err(api_err) => return api_err.into_response(),
    };

    // Inactive users would have been rejected at login, but a token minted
    // before deactivation remains cryptographically valid until it expires.
    // Handlers that need to enforce `active` should look up the user by
    // `ctx.user_id` if that guarantee is required; the middleware does not
    // do a DB round-trip on every request by design (keep 99th percentile low).
    request.extensions_mut().insert(ctx.clone());

    // Also make it available to `RestaurantContext::from_request_parts` if that
    // later prefers `CallerContext.restaurant` over `X-Restaurant`.
    let mut response = next.run(request).await;

    // Echo caller identity to downstream layers (logging) without leaking the token.
    response.extensions_mut().insert(ctx);
    response
}

// ---------------------------------------------------------------------------
// Extractor
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for CallerContext
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CallerContext>()
            .cloned()
            .ok_or_else(|| ApiError::unauthorized("missing authentication: peacock_session cookie required"))
    }
}

// ---------------------------------------------------------------------------
// require_role! macro
// ---------------------------------------------------------------------------

/// Enforces a minimum [`Role`] on a [`CallerContext`].
///
/// Hierarchy is `waiter < cashier < manager < owner < dev`; a caller with a
/// higher role satisfies a lower requirement. On failure returns early with
/// `Err(ApiError::forbidden(..))`, so it is used inside handlers returning
/// [`crate::error::ApiResult`].
///
/// ```ignore
/// async fn close_shift(caller: CallerContext) -> ApiResult<Json<Value>> {
///     require_role!(caller, Role::Manager);
///     // ... close ...
///     Ok(Json(json!({})))
/// }
/// ```
///
/// The macro is exported at the crate root (`crate::require_role!`) and also
/// re-exported as `crate::middleware::auth::require_role!` for module-qualified
/// use.
#[macro_export]
macro_rules! require_role {
    ($ctx:expr, $required:expr) => {
        if !$ctx.has_role($required) {
            return Err($crate::error::ApiError::forbidden(format!(
                "requires role {} but caller has {}",
                $required.as_str(),
                $ctx.role.as_str()
            )));
        }
    };
    ($ctx:expr, $required:expr, $msg:expr) => {
        if !$ctx.has_role($required) {
            return Err($crate::error::ApiError::forbidden($msg));
        }
    };
}

// Re-export under the module path so `crate::middleware::auth::require_role!` works.
// `#[macro_export]` already places `crate::require_role!` at the crate root.
pub use crate::require_role;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    fn headers_with_cookie(value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_str(value).unwrap(),
        );
        map
    }

    #[test]
    fn extracts_peacock_session_cookie_from_single_cookie_header() {
        let headers = headers_with_cookie("peacock_session=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.test; other=foo");
        assert_eq!(
            extract_peacock_session_cookie(&headers).unwrap(),
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.test"
        );
    }

    #[test]
    fn extracts_peacock_session_cookie_ignoring_spaces_and_quotes() {
        let headers = headers_with_cookie(" other=1; peacock_session=\"abc.def.ghi\" ; foo=bar ");
        assert_eq!(extract_peacock_session_cookie(&headers).unwrap(), "abc.def.ghi");
    }

    #[test]
    fn extracts_bearer_as_fallback() {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str("Bearer my.jwt.token").unwrap(),
        );
        assert_eq!(extract_token(&headers).unwrap(), "my.jwt.token");
    }

    #[test]
    fn cookie_wins_over_bearer() {
        let mut headers = headers_with_cookie("peacock_session=cookie.jwt.here");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str("Bearer bearer.jwt.here").unwrap(),
        );
        assert_eq!(extract_token(&headers).unwrap(), "cookie.jwt.here");
    }

    #[test]
    fn returns_none_when_cookie_absent() {
        let headers = headers_with_cookie("other=123; foo=bar");
        assert!(extract_peacock_session_cookie(&headers).is_none());
        assert!(extract_token(&headers).is_none());
    }

    #[test]
    fn role_hierarchy_is_owner_over_manager_over_cashier_over_waiter() {
        assert!(Role::Owner.has_level_over(Role::Manager));
        assert!(Role::Manager.has_level_over(Role::Cashier));
        assert!(Role::Cashier.has_level_over(Role::Waiter));
        assert!(!Role::Waiter.has_level_over(Role::Manager));
    }

    // helper for hierarchy test
    trait HasLevel {
        fn has_level_over(&self, other: Self) -> bool;
    }
    impl HasLevel for Role {
        fn has_level_over(&self, other: Self) -> bool {
            self.level() >= other.level()
        }
    }

    #[test]
    fn parses_roles_case_insensitive() {
        assert_eq!(Role::parse("Waiter"), Some(Role::Waiter));
        assert_eq!(Role::parse("MANAGER"), Some(Role::Manager));
        assert_eq!(Role::parse("owner"), Some(Role::Owner));
        assert_eq!(Role::parse("dev"), Some(Role::Dev));
        assert_eq!(Role::parse("unknown"), None);
    }

    #[test]
    fn verifies_and_rejects_jwt() {
        let secret = "test-jwt-secret-32-bytes-long-xxxx";
        let claims = Claims {
            sub: "user-123".to_string(),
            role: "manager".to_string(),
            restaurant: Some("Peacock Restaurant".to_string()),
            branch: Some("Main".to_string()),
            email: Some("m@peacock.local".to_string()),
            exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp() as usize,
            iat: Some(chrono::Utc::now().timestamp() as usize),
        };
        let token = mint_jwt(&claims, secret).expect("mint must succeed");
        let decoded = verify_jwt(&token, secret).expect("verify must succeed");
        assert_eq!(decoded.sub, "user-123");
        assert_eq!(decoded.role, "manager");

        // Wrong secret must fail.
        assert!(verify_jwt(&token, "wrong-secret-32-bytes-long-xxxxxxx").is_err());

        // Expired token must fail.
        let expired = Claims {
            exp: (chrono::Utc::now() - chrono::Duration::hours(1)).timestamp() as usize,
            ..claims
        };
        let expired_token = mint_jwt(&expired, secret).unwrap();
        assert!(verify_jwt(&expired_token, secret).is_err());
    }

    #[test]
    fn caller_context_from_claims_maps_role_and_outlet() {
        let claims = Claims {
            sub: "0196a3d4-7c4e-7000-8000-000000000001".to_string(),
            role: "owner".to_string(),
            restaurant: Some("Peacock Restaurant".to_string()),
            branch: Some("Main".to_string()),
            email: None,
            exp: 9999999999,
            iat: None,
        };
        let ctx = CallerContext::from_claims(claims).unwrap();
        assert_eq!(ctx.role, Role::Owner);
        assert!(ctx.has_role(Role::Manager));
        assert!(!ctx.has_role(Role::Dev));
        assert_eq!(ctx.restaurant.unwrap().as_str(), "Peacock Restaurant");
    }

    #[test]
    fn is_public_path_pins_health_and_auth() {
        assert!(is_public_path("/health"));
        assert!(is_public_path("/health/ready"));
        assert!(is_public_path("/api/auth/login"));
        assert!(is_public_path("/api/auth/logout"));
        assert!(is_public_path("/api/auth/pin-login"));
        assert!(!is_public_path("/api/tables"));
        assert!(!is_public_path("/api/menu"));
        assert!(!is_public_path("/api/orders"));
    }

    #[test]
    fn require_role_returns_403_not_401() {
        let caller = CallerContext {
            user_id: "0196a3d4-7c4e-7000-8000-000000000002".to_string(),
            email: None,
            role: Role::Waiter,
            restaurant: None,
            branch: None,
            exp: 9999999999,
        };
        let res: Result<(), crate::error::ApiError> = (|| {
            crate::require_role!(caller, Role::Owner);
            Ok(())
        })();
        let err = res.expect_err("waiter must not satisfy owner");
        assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
        assert_eq!(err.kind(), crate::error::ProblemKind::Forbidden);
        assert!(err.detail().contains("waiter"));
        assert!(err.detail().contains("owner"));
    }

    #[test]
    fn require_role_manager_passes_for_owner_and_dev() {
        for role in [Role::Owner, Role::Dev] {
            let caller = CallerContext {
                user_id: "1".into(),
                email: None,
                role,
                restaurant: None,
                branch: None,
                exp: 9999999999,
            };
            let res: Result<(), crate::error::ApiError> = (|| {
                crate::require_role!(caller, Role::Manager);
                Ok(())
            })();
            assert!(res.is_ok(), "{role:?} must satisfy manager");
        }
        // waiter must fail for manager
        let waiter = CallerContext {
            user_id: "2".into(),
            email: None,
            role: Role::Waiter,
            restaurant: None,
            branch: None,
            exp: 9999999999,
        };
        let res: Result<(), crate::error::ApiError> = (|| {
            crate::require_role!(waiter, Role::Manager);
            Ok(())
        })();
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().status(), axum::http::StatusCode::FORBIDDEN);
    }
}
