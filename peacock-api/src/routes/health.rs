//! Liveness and readiness endpoints.
//!
//! ## Two probes, because they answer two different questions
//!
//! `/health` is **liveness**: is this process serving HTTP. It touches nothing, so a
//! database blip cannot make an orchestrator kill a process that is working fine. Killing
//! healthy tills because Postgres hiccupped is strictly worse than leaving them up.
//!
//! `/health/ready` is **readiness**: can this process serve a *request*. It round-trips
//! `SELECT 1` through the pool, so a load balancer stops routing to a till whose database
//! is unreachable instead of handing it orders it will fail. It reports `503` in that case
//! — not `500`: the condition is transient and the caller should retry elsewhere.
//!
//! Wiring the database check into `/health` instead would collapse the two, and the
//! failure mode is asymmetric: a false "dead" restarts a working process mid-payment,
//! while a false "not ready" only sheds load. So they stay separate routes.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: &'static str,
}

/// Liveness. Dependency-free by design — see the module docs.
pub async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

/// What the readiness probe observed about the database.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessResponse {
    /// `"ready"` or `"unavailable"`.
    pub status: &'static str,
    pub database: DatabaseHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DatabaseHealth {
    /// `true` when `SELECT 1` round-tripped.
    pub connected: bool,
    /// Round-trip time in milliseconds. Absent when the check failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Connections currently held by the pool, idle or busy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_connections: Option<usize>,
    /// Why the check failed. Never carries the connection string — the storage layer
    /// redacts it before the error is built (`DbConfig::redacted_url`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How long the readiness check waits for the database before calling it unavailable.
///
/// Shorter than the pool's own acquire timeout (10s by default): a probe that hangs for
/// ten seconds has already failed as far as the load balancer is concerned, and holding
/// the connection that long during an outage adds contention to a pool that is already
/// struggling.
const READINESS_TIMEOUT: Duration = Duration::from_secs(2);

/// Readiness: `200` when the database answers, `503` when it does not.
pub async fn readiness_check(
    State(state): State<AppState>,
) -> (StatusCode, Json<ReadinessResponse>) {
    // Lane W1-A: there is no "no pool configured" case to report any more. A process that
    // could not reach a database refuses to start (`main.rs`), so the only way readiness
    // can fail is a pool that *was* live and stopped answering — which the match below
    // covers, and which is the case a load balancer actually needs to distinguish.
    let storage = state.storage();

    match tokio::time::timeout(READINESS_TIMEOUT, storage.health_check()).await {
        Ok(Ok(health)) => (
            StatusCode::OK,
            Json(ReadinessResponse {
                status: "ready",
                database: DatabaseHealth {
                    connected: true,
                    latency_ms: Some(health.latency.as_millis().min(u64::MAX as u128) as u64),
                    pool_size: Some(health.pool_size),
                    idle_connections: Some(health.idle_connections),
                    error: None,
                },
            }),
        ),
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "readiness: database health check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ReadinessResponse {
                    status: "unavailable",
                    database: DatabaseHealth {
                        connected: false,
                        latency_ms: None,
                        pool_size: None,
                        idle_connections: None,
                        error: Some(err.to_string()),
                    },
                }),
            )
        }
        Err(_elapsed) => {
            tracing::warn!(
                timeout_ms = READINESS_TIMEOUT.as_millis() as u64,
                "readiness: database health check timed out"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ReadinessResponse {
                    status: "unavailable",
                    database: DatabaseHealth {
                        connected: false,
                        latency_ms: None,
                        pool_size: None,
                        idle_connections: None,
                        error: Some(format!(
                            "database did not answer within {}ms",
                            READINESS_TIMEOUT.as_millis()
                        )),
                    },
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[tokio::test]
    async fn reports_ok() {
        let Json(body) = health_check().await;
        assert_eq!(body.status, "ok");
        assert_eq!(
            serde_json::to_string(&body).unwrap(),
            r#"{"status":"ok"}"#,
            "wire format is part of the contract"
        );
    }

    #[tokio::test]
    async fn liveness_does_not_depend_on_the_database() {
        // The whole point of the split: no pool, still alive.
        let Json(body) = health_check().await;
        assert_eq!(body.status, "ok");
    }

    #[tokio::test]
    async fn readiness_without_a_database_is_503_and_says_why() {
        let state = AppState::new(Config::default());
        let (status, Json(body)) = readiness_check(State(state)).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.status, "unavailable");
        assert!(!body.database.connected);
        assert!(
            body.database
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("DATABASE_URL"),
            "the error must tell an operator what to set: {:?}",
            body.database.error
        );
    }

    #[test]
    fn a_failed_readiness_check_omits_the_pool_numbers_rather_than_faking_them() {
        // Serialising `pool_size: 0` would read as "a pool with no connections", which is
        // a different fact from "we could not ask".
        let body = ReadinessResponse {
            status: "unavailable",
            database: DatabaseHealth {
                connected: false,
                latency_ms: None,
                pool_size: None,
                idle_connections: None,
                error: Some("connection refused".to_owned()),
            },
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["database"]["connected"], false);
        assert!(json["database"].get("pool_size").is_none());
        assert!(json["database"].get("latency_ms").is_none());
        assert_eq!(json["database"]["error"], "connection refused");
    }

    #[test]
    fn the_readiness_timeout_is_shorter_than_the_pools_acquire_timeout() {
        // A probe that outlasts the pool's own patience tells the load balancer nothing
        // it can act on in time.
        let pool_default = peacock_storage::DbConfig::from_url("postgres://localhost/x")
            .unwrap()
            .acquire_timeout;
        assert!(
            READINESS_TIMEOUT < pool_default,
            "readiness timeout {READINESS_TIMEOUT:?} must be under the pool's {pool_default:?}"
        );
    }
}
