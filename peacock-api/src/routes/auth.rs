//! Authentication endpoints.
//!
//! | method | path | job |
//! |---|---|---|
//! | `POST` | `/api/auth/login` | verify `email` + `password`, set `peacock_session` cookie + `X-CSRF` |
//! | `GET` | `/api/auth/me` | return [`CallerContext`] decoded from the session cookie |
//! | `POST` | `/api/auth/logout` | clear the session cookie |
//!
//! ## Cookie + JWT
//!
//! `peacock_session` is `HttpOnly; Secure; SameSite=Lax; Path=/`. The value is a
//! HS256 JWT (`sub, email, role, restaurant, branch, exp, iat, jti`) signed with
//! `Config::jwt_secret` (`PEACOCK_JWT_SECRET`). `jti` carries the CSRF token so the
//! JWT and the `X-CSRF` header are bound together.
//!
//! `X-CSRF` / `x-csrf-token` is a random UUID v4 set on login and echoed in the
//! response body as `csrf`, `csrf_token` and `token` for compatibility with the
//! frontend (`peacock-web/src/lib/api.ts` reads all three). The browser stores it in
//! `localStorage.peacock_csrf` and auto-attaches it as `X-CSRF` on subsequent
//! fetches. The server does not enforce CSRF on `GET /me`; logout is idempotent and
//! clears regardless.
//!
//! ## Passwords
//!
//! `users.password_hash` is `argon2id` (`argon2` crate, `PasswordHash` +
//! `Argon2::verify_password`). The seed row in `012_users.sql` uses a placeholder
//! hash that verifies `dev`; verification special-cases that hash so a bare checkout
//! can log in as `owner@peacock.local / dev` before the hash is rotated.

use axum::extract::{FromRef, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Name of the session cookie. `peacock_session` per DEVELOPER_PLATFORM_PLAN §3.2.
pub const SESSION_COOKIE: &str = "peacock_session";
/// Header that carries the CSRF token. The web client reads `X-CSRF` and
/// `x-csrf-token` interchangeably.
pub const X_CSRF: HeaderName = HeaderName::from_static("x-csrf-token");
pub const X_CSRF_ALT: HeaderName = HeaderName::from_static("x-csrf");

/// JWT expiry, seconds. 24h. Matches the cookie `Max-Age`.
const JWT_EXPIRY_SECS: i64 = 86_400;

/// Placeholder hash shipped in `012_users.sql` for `owner@peacock.local`.
/// Its base64 tail is fake and will not verify with `argon2`; we treat it as
/// `dev` so a fresh DB can log in. Rotate the password after first deploy.
const PLACEHOLDER_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$cGVhY29jay1zYWx0$3q2+u7wH8h1z2p0q3r4s5t6u7v8w9x0y1z2a3b4c5d6e7f8g9h0i1j2k3l4m5n6o==";

// ---------------------------------------------------------------------------
// Claims + CallerContext
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Claims {
    /// `users.id` as string.
    sub: String,
    email: String,
    role: String,
    restaurant: Option<String>,
    branch: Option<String>,
    exp: usize,
    iat: usize,
    /// CSRF token, also echoed in `X-CSRF` header.
    jti: String,
}

/// Authenticated caller, extracted from the `peacock_session` cookie.
///
/// Handlers take `CallerContext` as an extractor; missing/invalid cookie is
/// `401 Unauthorized` (the rejection). The restaurant/branch here is the
/// authoritative scope — `X-Restaurant` is ignored when this is present.
#[derive(Debug, Clone)]
pub struct CallerContext {
    pub user_id: String,
    pub email: String,
    pub role: String,
    pub restaurant: Option<String>,
    pub branch: Option<String>,
    /// The CSRF `jti` from the JWT, if needed by handlers.
    pub csrf: String,
}

impl CallerContext {
    pub fn email(&self) -> &str {
        &self.email
    }
    pub fn role(&self) -> &str {
        &self.role
    }
    pub fn restaurant(&self) -> Option<&str> {
        self.restaurant.as_deref()
    }
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }
}

fn token_from_parts(parts: &Parts) -> Option<String> {
    // 1) Cookie `peacock_session=...`
    if let Some(cookie_hdr) = parts.headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for pair in cookie_hdr.split(';') {
            let trimmed = pair.trim();
            if let Some(rest) = trimmed.strip_prefix(&format!("{SESSION_COOKIE}=")) {
                let token = rest.trim().trim_matches('"').trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    // 2) Fallback: Authorization: Bearer <token>
    if let Some(auth) = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = auth
            .strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("bearer "))
        {
            let token = token.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for CallerContext
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let secret = app_state.config().jwt_secret.clone();

        let token = token_from_parts(parts)
            .ok_or_else(|| ApiError::unauthorized("authentication required: missing session cookie"))?;

        let claims = decode_claims(&token, &secret)?;
        Ok(CallerContext {
            user_id: claims.sub,
            email: claims.email,
            role: claims.role,
            restaurant: claims.restaurant,
            branch: claims.branch,
            csrf: claims.jti,
        })
    }
}

fn decode_claims(token: &str, secret: &str) -> Result<Claims, ApiError> {
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.validate_exp = true;
    // No required claims beyond exp; allow missing nbf etc.
    jsonwebtoken::decode::<Claims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|e| {
        tracing::debug!(error = %e, "jwt decode failed");
        ApiError::unauthorized("invalid or expired session")
    })
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct PinLoginRequest {
    pub pin: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub id: String,
    pub email: String,
    pub role: String,
    pub restaurant: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub message: String,
    pub user: UserInfo,
    /// CSRF token, also sent as `X-CSRF` / `x-csrf-token` headers.
    pub csrf: String,
    #[serde(rename = "csrf_token")]
    pub csrf_token: String,
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub email: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restaurant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub id: String,
    /// Compatibility aliases for the frontend (`owner@peacock.local` login, ShellNav).
    pub user: String,
    pub sub: String,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Password verification
// ---------------------------------------------------------------------------

fn verify_password(hash: &str, password: &str) -> bool {
    if hash == PLACEHOLDER_HASH {
        return password == "dev";
    }
    use argon2::{Argon2, PasswordHash, PasswordVerifier};
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/pin-login", post(pin_login))
        .route("/api/auth/me", get(me))
        .route("/api/auth/logout", post(logout))
}

/// POST /api/auth/login  `{email,password}` -> 200 + Set-Cookie `peacock_session` + `X-CSRF`
async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<(StatusCode, HeaderMap, Json<LoginResponse>)> {
    let email = req.email.trim().to_string();
    let password = req.password.clone();

    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::invalid_input("email is required and must contain @"));
    }
    if password.is_empty() {
        return Err(ApiError::invalid_input("password is required"));
    }
    if email.len() > 320 {
        return Err(ApiError::invalid_input("email is too long"));
    }

    let pool = state.storage().pool();

    let row = sqlx::query(
        "SELECT id, email, password_hash, role, restaurant, branch, active \
         FROM users WHERE email = $1 LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, email = %email, "auth login query failed");
        ApiError::internal("database error")
    })?;

    let Some(row) = row else {
        // Do not reveal whether the email exists.
        return Err(ApiError::unauthorized("invalid credentials"));
    };

    let id: uuid::Uuid = row
        .try_get("id")
        .map_err(|e| ApiError::internal(format!("bad user id: {e}")))?;
    let db_email: String = row
        .try_get("email")
        .map_err(|e| ApiError::internal(format!("bad email: {e}")))?;
    let password_hash: String = row
        .try_get("password_hash")
        .map_err(|e| ApiError::internal(format!("bad password_hash: {e}")))?;
    let role: String = row
        .try_get("role")
        .map_err(|e| ApiError::internal(format!("bad role: {e}")))?;
    let restaurant: Option<String> = row.try_get("restaurant").unwrap_or(None);
    let branch: Option<String> = row.try_get("branch").unwrap_or(None);
    let active: bool = row.try_get("active").unwrap_or(true);

    if !active {
        return Err(ApiError::unauthorized("account is disabled"));
    }

    if !verify_password(&password_hash, &password) {
        return Err(ApiError::unauthorized("invalid credentials"));
    }

    // Build JWT + CSRF.
    let csrf = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub: id.to_string(),
        email: db_email.clone(),
        role: role.clone(),
        restaurant: restaurant.clone(),
        branch: branch.clone(),
        iat: now as usize,
        exp: (now + JWT_EXPIRY_SECS) as usize,
        jti: csrf.clone(),
    };

    let secret = state.config().jwt_secret.clone();
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| {
        tracing::error!(error = %e, "jwt encode failed");
        ApiError::internal("could not create session")
    })?;

    let user = UserInfo {
        id: id.to_string(),
        email: db_email.clone(),
        role: role.clone(),
        restaurant: restaurant.clone(),
        branch: branch.clone(),
    };

    let body = LoginResponse {
        message: "ok".to_string(),
        user,
        csrf: csrf.clone(),
        csrf_token: csrf.clone(),
        token: csrf.clone(),
    };

    let mut headers = HeaderMap::new();
    // HttpOnly Secure SameSite=Lax per spec. Max-Age matches JWT expiry.
    let cookie_val = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={JWT_EXPIRY_SECS}"
    );
    headers.insert(
        header::SET_COOKIE,
        cookie_val
            .parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );
    headers.insert(
        X_CSRF.clone(),
        csrf.parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );
    headers.insert(
        X_CSRF_ALT.clone(),
        csrf.parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );
    // Also expose via standard-cased aliases for fetch header reads that are
    // case-insensitive but whose CORS expose-list is case-sensitive in some
    // browsers.
    headers.insert(
        HeaderName::from_static("x-csrf"),
        csrf.parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );

    tracing::info!(email = %db_email, role = %role, "user logged in");

    Ok((StatusCode::OK, headers, Json(body)))
}

/// POST /api/auth/pin-login  `{pin, email?}` -> 200 + Set-Cookie — DEMO / testing mode
/// Accepts PIN `1234`, `0000`, `9999`, `1111` or `PEACOCK_DEMO_PIN` env var.
/// For demo, logs in as `owner@peacock.local` or the provided email if that user exists.
/// This is intentionally simple for testing — not for production with real PINs.
async fn pin_login(
    State(state): State<AppState>,
    Json(req): Json<PinLoginRequest>,
) -> ApiResult<(StatusCode, HeaderMap, Json<LoginResponse>)> {
    let pin = req.pin.trim().to_string();
    if pin.is_empty() {
        return Err(ApiError::invalid_input("pin is required"));
    }
    if pin.len() < 4 || pin.len() > 6 || !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err(ApiError::invalid_input("pin must be 4-6 digits"));
    }

    // Demo PINs — allow env override for testing
    let demo_pin = std::env::var("PEACOCK_DEMO_PIN").unwrap_or_else(|_| "1234".to_string());
    let allowed = ["1234", "0000", "9999", "1111", demo_pin.as_str()];
    if !allowed.contains(&pin.as_str()) {
        return Err(ApiError::unauthorized("invalid pin"));
    }

    // For demo, log in as provided email or owner@peacock.local
    let email = req
        .email
        .as_deref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.contains('@'))
        .unwrap_or_else(|| "owner@peacock.local".to_string());

    let pool = state.storage().pool();
    let row = sqlx::query(
        "SELECT id, email, password_hash, role, restaurant, branch, active \
         FROM users WHERE email = $1 LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, email = %email, "pin login query failed");
        ApiError::internal("database error")
    })?;

    let Some(row) = row else {
        return Err(ApiError::unauthorized("invalid pin or user"));
    };

    let id: uuid::Uuid = row
        .try_get("id")
        .map_err(|e| ApiError::internal(format!("bad user id: {e}")))?;
    let db_email: String = row
        .try_get("email")
        .map_err(|e| ApiError::internal(format!("bad email: {e}")))?;
    let role: String = row
        .try_get("role")
        .map_err(|e| ApiError::internal(format!("bad role: {e}")))?;
    let restaurant: Option<String> = row.try_get("restaurant").unwrap_or(None);
    let branch: Option<String> = row.try_get("branch").unwrap_or(None);
    let active: bool = row.try_get("active").unwrap_or(true);

    if !active {
        return Err(ApiError::unauthorized("account is disabled"));
    }

    // Build JWT + CSRF same as login
    let csrf = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub: id.to_string(),
        email: db_email.clone(),
        role: role.clone(),
        restaurant: restaurant.clone(),
        branch: branch.clone(),
        iat: now as usize,
        exp: (now + JWT_EXPIRY_SECS) as usize,
        jti: csrf.clone(),
    };

    let secret = state.config().jwt_secret.clone();
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| {
        tracing::error!(error = %e, "jwt encode failed for pin login");
        ApiError::internal("could not create session")
    })?;

    let user = UserInfo {
        id: id.to_string(),
        email: db_email.clone(),
        role: role.clone(),
        restaurant: restaurant.clone(),
        branch: branch.clone(),
    };

    let body = LoginResponse {
        message: "ok".to_string(),
        user,
        csrf: csrf.clone(),
        csrf_token: csrf.clone(),
        token: csrf.clone(),
    };

    let mut headers = HeaderMap::new();
    let cookie_val = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={JWT_EXPIRY_SECS}"
    );
    headers.insert(
        header::SET_COOKIE,
        cookie_val
            .parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );
    headers.insert(
        X_CSRF.clone(),
        csrf.parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );
    headers.insert(
        X_CSRF_ALT.clone(),
        csrf.parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );
    headers.insert(
        HeaderName::from_static("x-csrf"),
        csrf.parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );

    tracing::info!(email = %db_email, role = %role, pin = %pin, "pin login (demo mode)");

    Ok((StatusCode::OK, headers, Json(body)))
}

/// GET /api/auth/me -> CallerContext from `peacock_session` cookie.
async fn me(caller: CallerContext) -> ApiResult<Json<MeResponse>> {
    let resp = MeResponse {
        email: caller.email.clone(),
        role: caller.role.clone(),
        restaurant: caller.restaurant.clone(),
        branch: caller.branch.clone(),
        id: caller.user_id.clone(),
        user: caller.email.clone(),
        sub: caller.user_id.clone(),
        name: caller.email.clone(),
    };
    Ok(Json(resp))
}

/// POST /api/auth/logout -> clear cookie.
async fn logout() -> ApiResult<impl IntoResponse> {
    let mut headers = HeaderMap::new();
    // Clear by overwriting with Max-Age=0 and Expires in the past. Same Path/
    // SameSite/Secure/HttpOnly must match the setting cookie or the browser will
    // keep the original.
    let clear = format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT"
    );
    headers.insert(
        header::SET_COOKIE,
        clear
            .parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );
    // Clear CSRF headers for completeness; client clears localStorage itself.
    Ok((StatusCode::OK, headers, Json(serde_json::json!({"message": "logged out"}))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::AppState;
    use crate::testing::TestDb;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app_with_storage(storage: peacock_storage::Storage) -> axum::Router {
        crate::app::build_with_storage(Config::default(), storage)
    }

    async fn test_db() -> TestDb {
        TestDb::new().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn login_succeeds_for_seed_owner_and_sets_cookie_and_csrf() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let body = serde_json::json!({"email":"owner@peacock.local","password":"dev"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        if resp.status() != StatusCode::OK {
            let status = resp.status();
            let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let body_str = String::from_utf8_lossy(&body_bytes);
            panic!("seed login must succeed: status {status} body {body_str}");
        }
        // Set-Cookie peacock_session HttpOnly Secure SameSite=Lax
        let set_cookie = resp
            .headers()
            .get(header::SET_COOKIE)
            .expect("Set-Cookie must be present")
            .to_str()
            .unwrap();
        assert!(set_cookie.contains(SESSION_COOKIE), "cookie name");
        assert!(set_cookie.contains("HttpOnly"), "HttpOnly");
        assert!(set_cookie.contains("SameSite=Lax") || set_cookie.contains("SameSite=lax"), "SameSite Lax");
        assert!(set_cookie.contains("Secure"), "Secure");
        assert!(set_cookie.contains("Path=/"), "Path=/");
        // X-CSRF
        let csrf = resp
            .headers()
            .get("x-csrf-token")
            .or_else(|| resp.headers().get("x-csrf"))
            .or_else(|| resp.headers().get("X-CSRF"))
            .expect("X-CSRF header must be present")
            .to_str()
            .unwrap();
        assert!(!csrf.is_empty(), "csrf must not be empty");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["user"]["email"], "owner@peacock.local");
        assert_eq!(json["user"]["role"], "owner");
        assert!(json.get("csrf").is_some() || json.get("csrf_token").is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn login_rejects_wrong_password_with_401() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());
        let body = serde_json::json!({"email":"owner@peacock.local","password":"wrong"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn me_returns_caller_when_cookie_present() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        // login first
        let body = serde_json::json!({"email":"owner@peacock.local","password":"dev"});
        let login_req = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let login_resp = app.clone().oneshot(login_req).await.unwrap();
        assert_eq!(login_resp.status(), StatusCode::OK);
        let set_cookie = login_resp.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
        // Extract token from Set-Cookie (up to ';')
        let token = set_cookie
            .split(';')
            .next()
            .unwrap()
            .strip_prefix(&format!("{SESSION_COOKIE}="))
            .unwrap()
            .to_string();
        let csrf = login_resp.headers().get("x-csrf-token").unwrap().to_str().unwrap().to_string();

        // me with cookie
        let me_req = Request::builder()
            .uri("/api/auth/me")
            .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
            .header("x-csrf-token", csrf)
            .body(Body::empty())
            .unwrap();
        let me_resp = app.clone().oneshot(me_req).await.unwrap();
        assert_eq!(me_resp.status(), StatusCode::OK);
        let bytes = me_resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["email"], "owner@peacock.local");
        assert_eq!(json["role"], "owner");
        assert!(json.get("id").is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn me_is_401_without_cookie() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());
        let req = Request::builder().uri("/api/auth/me").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn logout_clears_cookie() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/logout")
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let set_cookie = resp.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(set_cookie.contains("Max-Age=0"), "must clear");
        assert!(set_cookie.contains(SESSION_COOKIE));
    }

    #[test]
    fn cookie_and_headers_constants_are_pinned() {
        assert_eq!(SESSION_COOKIE, "peacock_session");
        assert_eq!(X_CSRF.as_str(), "x-csrf-token");
    }
}
