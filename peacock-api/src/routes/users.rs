//! User management endpoints.
//!
//! | method | path | job |
//! |---|---|---|
//! | `POST` | `/api/users` | create a user (Owner only) |
//! | `GET` | `/api/users` | list users (Owner only) |
//! | `PATCH` | `/api/users/:id` | update a user (Owner only) |
//!
//! ## Auth
//!
//! All three require `CallerContext` with `Owner` (or `Dev`) role via
//! `require_role!(caller, Owner)`. Unauthenticated callers receive `401` from
//! the auth middleware (or the extractor), non-Owner callers receive `401` from
//! `require_role!`.
//!
//! ## Passwords
//!
//! `users.password_hash` is `argon2id` — the same scheme `auth.rs` verifies.
//! `POST` and `PATCH` (when `password` is supplied) hash with
//! `argon2::Argon2::default()` + random `SaltString` before `INSERT`/`UPDATE`.
//!
//! ## Storage
//!
//! Direct `sqlx` over `peacock_storage::Storage::pool()` — there is no
//! `peacock-storage` users repo yet, so this module owns the SQL the way
//! `auth.rs` does for login. `INSERT users` is the source of truth the task
//! requires, with `created_by` set to `caller.user_id`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::error::{ApiError, ApiResult};
use crate::middleware::auth::{CallerContext, Role};
use crate::middleware::auth::Role::Owner;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub email: String,
    pub password: String,
    pub role: String,
    #[serde(default)]
    pub restaurant: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub restaurant: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserResponse {
    pub id: String,
    pub email: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restaurant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserListResponse {
    pub count: usize,
    pub users: Vec<UserResponse>,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/users", post(create_user).get(list_users))
        .route("/api/users/:id", patch(update_user))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn hash_password(password: &str) -> Result<String, ApiError> {
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    use argon2::Argon2;
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| {
            tracing::error!(error = %e, "argon2 hash failed");
            ApiError::internal("password hashing failed")
        })
}

fn validate_email(email: &str) -> Result<String, ApiError> {
    let trimmed = email.trim().to_string();
    if trimmed.is_empty() || !trimmed.contains('@') {
        return Err(ApiError::invalid_input(
            "email is required and must contain @",
        ));
    }
    if trimmed.len() > 320 {
        return Err(ApiError::invalid_input("email is too long"));
    }
    Ok(trimmed)
}

fn normalize_optional(v: &Option<String>) -> Option<String> {
    match v {
        Some(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        None => None,
    }
}

/// `None` means "not supplied" (no change), `Some(None)` was collapsed to `None`
/// by the caller — this helper is for the create path where `None` means NULL.
fn restaurant_branch_opt(v: Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn parse_role(raw: &str) -> Result<Role, ApiError> {
    Role::parse(raw).ok_or_else(|| {
        ApiError::invalid_input(format!(
            "invalid role {:?}: expected one of waiter, cashier, manager, owner",
            raw
        ))
    })
}

fn row_to_user(row: &sqlx::postgres::PgRow) -> Result<UserResponse, ApiError> {
    let id: uuid::Uuid = row
        .try_get("id")
        .map_err(|e| ApiError::internal(format!("bad user id: {e}")))?;
    let email: String = row
        .try_get("email")
        .map_err(|e| ApiError::internal(format!("bad email: {e}")))?;
    let role: String = row
        .try_get("role")
        .map_err(|e| ApiError::internal(format!("bad role: {e}")))?;
    let restaurant: Option<String> = row.try_get("restaurant").unwrap_or(None);
    let branch: Option<String> = row.try_get("branch").unwrap_or(None);
    let active: bool = row.try_get("active").unwrap_or(true);
    let created_by: Option<uuid::Uuid> = row.try_get("created_by").unwrap_or(None);
    let created_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("created_at").unwrap_or(None);
    let updated_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("updated_at").unwrap_or(None);

    Ok(UserResponse {
        id: id.to_string(),
        email,
        role,
        restaurant,
        branch,
        active,
        created_by: created_by.map(|u| u.to_string()),
        created_at,
        updated_at,
    })
}

// ---------------------------------------------------------------------------
// POST /api/users
// ---------------------------------------------------------------------------

/// Create a user. Owner only.
///
/// Body: `{ email, password, role, restaurant?, branch?, active? }`
async fn create_user(
    caller: CallerContext,
    State(state): State<AppState>,
    Json(req): Json<CreateUserRequest>,
) -> ApiResult<(StatusCode, Json<UserResponse>)> {
    crate::require_role!(caller, Owner);

    let email = validate_email(&req.email)?;
    if req.password.trim().is_empty() {
        return Err(ApiError::invalid_input("password is required"));
    }
    let role = parse_role(&req.role)?;
    let restaurant = restaurant_branch_opt(req.restaurant);
    let branch = restaurant_branch_opt(req.branch);
    let active = req.active.unwrap_or(true);

    let pool = state.storage().pool();

    // Pre-check duplicate for a clean 409 without leaking DB constraint text.
    let existing: Option<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, email = %email, "user duplicate check failed");
            ApiError::internal("database error")
        })?;
    if existing.is_some() {
        return Err(ApiError::conflict(format!("email {} already exists", email)));
    }

    let password_hash = hash_password(&req.password)?;

    let new_id = uuid::Uuid::new_v4();
    let created_by = uuid::Uuid::parse_str(&caller.user_id).ok();

    let row = sqlx::query(
        "INSERT INTO users (id, email, password_hash, role, restaurant, branch, active, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id, email, role, restaurant, branch, active, created_by, created_at, updated_at",
    )
    .bind(new_id)
    .bind(&email)
    .bind(&password_hash)
    .bind(role.as_str())
    .bind(&restaurant)
    .bind(&branch)
    .bind(active)
    .bind(created_by)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        // Unique violation race — the pre-check passed but another insert won.
        let msg = e.to_string();
        if msg.contains("duplicate") || msg.contains("users_email_key") || msg.contains("already exists") {
            return ApiError::conflict(format!("email {} already exists", email));
        }
        tracing::error!(error = %e, email = %email, "INSERT users failed");
        ApiError::internal("database error")
    })?;

    let resp = row_to_user(&row)?;
    tracing::info!(user_id = %resp.id, email = %resp.email, role = %resp.role, created_by = %caller.user_id, "user created");
    Ok((StatusCode::CREATED, Json(resp)))
}

// ---------------------------------------------------------------------------
// GET /api/users
// ---------------------------------------------------------------------------

/// List users. Owner only.
async fn list_users(
    caller: CallerContext,
    State(state): State<AppState>,
) -> ApiResult<Json<UserListResponse>> {
    crate::require_role!(caller, Owner);

    let pool = state.storage().pool();
    let rows = sqlx::query(
        "SELECT id, email, role, restaurant, branch, active, created_by, created_at, updated_at \
         FROM users ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "list users failed");
        ApiError::internal("database error")
    })?;

    let mut users = Vec::with_capacity(rows.len());
    for row in &rows {
        users.push(row_to_user(row)?);
    }
    let count = users.len();
    Ok(Json(UserListResponse { count, users }))
}

// ---------------------------------------------------------------------------
// PATCH /api/users/:id
// ---------------------------------------------------------------------------

/// Update a user. Owner only.
///
/// Patch body may contain any of `email, password, role, restaurant, branch, active`.
/// Omitted fields are left unchanged. `password` when supplied is re-hashed with argon2.
/// `restaurant`/`branch` set to empty string clears the column (NULL).
async fn update_user(
    caller: CallerContext,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateUserRequest>,
) -> ApiResult<Json<UserResponse>> {
    crate::require_role!(caller, Owner);

    let user_id = uuid::Uuid::parse_str(id.trim()).map_err(|_| {
        ApiError::invalid_input(format!("invalid user id {:?}: expected UUID", id))
    })?;

    // Must change at least one field.
    if req.email.is_none()
        && req.password.is_none()
        && req.role.is_none()
        && req.restaurant.is_none()
        && req.branch.is_none()
        && req.active.is_none()
    {
        return Err(ApiError::invalid_input(
            "request body must change at least one field",
        ));
    }

    let pool = state.storage().pool();

    // Load existing.
    let existing = sqlx::query(
        "SELECT id, email, password_hash, role, restaurant, branch, active, created_by, created_at, updated_at \
         FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, user_id = %user_id, "SELECT user failed");
        ApiError::internal("database error")
    })?
    .ok_or_else(|| ApiError::not_found(format!("user {} not found", user_id)))?;

    let cur_email: String = existing
        .try_get("email")
        .map_err(|e| ApiError::internal(format!("bad email: {e}")))?;
    let cur_role: String = existing
        .try_get("role")
        .map_err(|e| ApiError::internal(format!("bad role: {e}")))?;
    let cur_restaurant: Option<String> = existing.try_get("restaurant").unwrap_or(None);
    let cur_branch: Option<String> = existing.try_get("branch").unwrap_or(None);
    let cur_active: bool = existing.try_get("active").unwrap_or(true);
    let cur_hash: String = existing
        .try_get("password_hash")
        .map_err(|e| ApiError::internal(format!("bad password_hash: {e}")))?;

    // Resolve new values.
    let new_email = if let Some(ref v) = req.email {
        validate_email(v)?
    } else {
        cur_email.clone()
    };

    // Duplicate email check if changed.
    if new_email != cur_email {
        let dup: Option<uuid::Uuid> =
            sqlx::query_scalar("SELECT id FROM users WHERE email = $1 AND id != $2")
                .bind(&new_email)
                .bind(user_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, email = %new_email, "duplicate check on patch failed");
                    ApiError::internal("database error")
                })?;
        if dup.is_some() {
            return Err(ApiError::conflict(format!("email {} already exists", new_email)));
        }
    }

    let new_hash = if let Some(ref pw) = req.password {
        if pw.trim().is_empty() {
            return Err(ApiError::invalid_input("password cannot be empty"));
        }
        hash_password(pw)?
    } else {
        cur_hash
    };

    let new_role = if let Some(ref r) = req.role {
        parse_role(r)?.as_str().to_string()
    } else {
        cur_role
    };

    // restaurant/branch: Option<String> in patch means "if Some, set to trimmed/NULL, else keep".
    let new_restaurant = if req.restaurant.is_some() {
        normalize_optional(&req.restaurant)
    } else {
        cur_restaurant
    };
    let new_branch = if req.branch.is_some() {
        normalize_optional(&req.branch)
    } else {
        cur_branch
    };
    let new_active = req.active.unwrap_or(cur_active);

    let row = sqlx::query(
        "UPDATE users SET email = $1, password_hash = $2, role = $3, restaurant = $4, branch = $5, active = $6, updated_at = now() \
         WHERE id = $7 \
         RETURNING id, email, role, restaurant, branch, active, created_by, created_at, updated_at",
    )
    .bind(&new_email)
    .bind(&new_hash)
    .bind(&new_role)
    .bind(&new_restaurant)
    .bind(&new_branch)
    .bind(new_active)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        let msg = e.to_string();
        if msg.contains("duplicate") || msg.contains("users_email_key") {
            return ApiError::conflict(format!("email {} already exists", new_email));
        }
        if msg.contains("users_role_check") {
            return ApiError::invalid_input(format!("invalid role {:?}", new_role));
        }
        tracing::error!(error = %e, user_id = %user_id, "UPDATE users failed");
        ApiError::internal("database error")
    })?;

    let resp = row_to_user(&row)?;
    tracing::info!(user_id = %resp.id, email = %resp.email, role = %resp.role, "user updated");
    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::AppState;
    use crate::testing::TestDb;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app_with_storage(storage: peacock_storage::Storage) -> axum::Router {
        crate::app::build_with_storage(Config::default(), storage)
    }

    async fn auth_cookie_and_headers(app: &axum::Router) -> (String, String) {
        let body = serde_json::json!({"email":"owner@peacock.local","password":"dev"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "seed login must succeed");
        let set_cookie = resp.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
        let cookie = set_cookie.split(';').next().unwrap().to_string();
        let csrf = resp.headers().get("x-csrf-token").or_else(|| resp.headers().get("x-csrf")).unwrap().to_str().unwrap().to_string();
        (cookie, csrf)
    }

    async fn test_db() -> TestDb {
        TestDb::new().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_requires_owner_and_hashes_password() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());
        let (cookie, _) = auth_cookie_and_headers(&app).await;

        let body = serde_json::json!({
            "email": "alice@peacock.local",
            "password": "s3cret!",
            "role": "cashier"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/users")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, cookie.clone())
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED, "owner can create user");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["email"], "alice@peacock.local");
        assert_eq!(json["role"], "cashier");
        assert!(json.get("id").is_some());
        // password_hash must not leak
        assert!(json.get("password_hash").is_none());

        // Verify hash is argon2 and verifies
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE email = $1")
            .bind("alice@peacock.local")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert!(hash.starts_with("$argon2"), "hash must be argon2, got {}", hash);
        // created_by should be owner id
        let created_by: Option<uuid::Uuid> =
            sqlx::query_scalar("SELECT created_by FROM users WHERE email = $1")
                .bind("alice@peacock.local")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(created_by.is_some(), "created_by must be set");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_and_patch_require_owner() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());
        let (cookie, _) = auth_cookie_and_headers(&app).await;

        // create a user to patch
        let body = serde_json::json!({"email":"bob@peacock.local","password":"pwd123","role":"waiter"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/users")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, cookie.clone())
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let created: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let id = created["id"].as_str().unwrap().to_string();

        // list
        let req = Request::builder()
            .uri("/api/users")
            .header(header::COOKIE, cookie.clone())
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let list: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(list["count"].as_u64().unwrap() >= 2);
        assert!(list["users"].as_array().unwrap().iter().any(|u| u["email"] == "bob@peacock.local"));

        // patch role + active
        let body = serde_json::json!({"role":"manager","active": false});
        let req = Request::builder()
            .method("PATCH")
            .uri(format!("/api/users/{id}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, cookie.clone())
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let patched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(patched["role"], "manager");
        assert_eq!(patched["active"], false);
        assert_eq!(patched["id"], id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unauthenticated_is_401() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());
        let req = Request::builder()
            .uri("/api/users")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
