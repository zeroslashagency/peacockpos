//! HTTP error mapping: domain errors in, RFC 7807 Problem Details out.
//!
//! Two halves:
//!
//! - [`ApiError`] is what handlers return. It carries a [`ProblemKind`] (status +
//!   stable `type` slug + human title) and a `detail` string.
//! - [`ProblemDetails`] is the wire format. `instance` and `request_id` are not known
//!   to the handler, so [`crate::middleware::error`] fills them in on the way out.
//!
//! `Unauthorized` lives here rather than in `peacock_core::Error`: authentication is an
//! HTTP concern and the domain crate has no notion of a caller identity.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use peacock_core::Error as DomainError;

/// RFC 7807 media type. Clients branch on this, so it must not drift to
/// `application/json`.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// The closed set of failure classes the API exposes.
///
/// Keeping this separate from [`ApiError`] means the `type` URI and `title` for a class
/// are defined once; a handler cannot invent an ad-hoc pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemKind {
    NotFound,
    AlreadyExists,
    Conflict,
    InvalidInput,
    Unauthorized,
    Internal,
}

impl ProblemKind {
    pub fn status(self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::AlreadyExists | Self::Conflict => StatusCode::CONFLICT,
            Self::InvalidInput => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable slug appended to the configured problem base URI.
    pub fn slug(self) -> &'static str {
        match self {
            Self::NotFound => "not-found",
            Self::AlreadyExists => "already-exists",
            Self::Conflict => "conflict",
            Self::InvalidInput => "invalid-input",
            Self::Unauthorized => "unauthorized",
            Self::Internal => "internal-error",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::NotFound => "Resource Not Found",
            Self::AlreadyExists => "Resource Already Exists",
            Self::Conflict => "Conflict",
            Self::InvalidInput => "Invalid Input",
            Self::Unauthorized => "Unauthorized",
            Self::Internal => "Internal Server Error",
        }
    }

    /// Best-effort reverse mapping for error responses produced outside our handlers
    /// (router 404, method-not-allowed, extractor rejections).
    pub fn from_status(status: StatusCode) -> Self {
        match status {
            StatusCode::NOT_FOUND => Self::NotFound,
            StatusCode::UNAUTHORIZED => Self::Unauthorized,
            StatusCode::CONFLICT => Self::Conflict,
            _ if status.is_client_error() => Self::InvalidInput,
            _ => Self::Internal,
        }
    }
}

/// The error type handlers return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    kind: ProblemKind,
    detail: String,
}

impl ApiError {
    pub fn new(kind: ProblemKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(ProblemKind::NotFound, detail)
    }

    pub fn already_exists(detail: impl Into<String>) -> Self {
        Self::new(ProblemKind::AlreadyExists, detail)
    }

    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(ProblemKind::Conflict, detail)
    }

    pub fn invalid_input(detail: impl Into<String>) -> Self {
        Self::new(ProblemKind::InvalidInput, detail)
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(ProblemKind::Unauthorized, detail)
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(ProblemKind::Internal, detail)
    }

    pub fn kind(&self) -> ProblemKind {
        self.kind
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn status(&self) -> StatusCode {
        self.kind.status()
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.title(), self.detail)
    }
}

impl std::error::Error for ApiError {}

impl From<DomainError> for ApiError {
    /// Classifies every `peacock_core::Error` variant.
    ///
    /// Exhaustive on purpose: a new domain variant should fail to compile here rather
    /// than silently degrade to 500 in production.
    fn from(err: DomainError) -> Self {
        let kind = match &err {
            // Missing resources.
            DomainError::TableNotFound(_) 
            | DomainError::NoActiveMenu
            | DomainError::ShiftNotFound(_)
            | DomainError::NoOpenShift(_) => ProblemKind::NotFound,

            // State conflicts: the request is well formed, the world disagrees.
            DomainError::AlreadyMerged(_)
            | DomainError::ShiftAlreadyOpen(_) => ProblemKind::AlreadyExists,
            DomainError::TableOccupied(_)
            | DomainError::MultipleActiveOrders { .. }
            | DomainError::Conflict { .. } => ProblemKind::Conflict,

            // Caller asked for something the rules forbid.
            DomainError::CrossRoomMerge { .. } => ProblemKind::InvalidInput,

            // Configuration or stored-data faults. The caller cannot fix these by
            // changing the request, so they are server-side failures.
            DomainError::BomZeroQuantity(_)
            | DomainError::SeriesNotConfigured(_, _)
            | DomainError::InvoiceNameTooLong { .. }
            | DomainError::NonNumericData { .. } => ProblemKind::Internal,
        };
        Self::new(kind, err.to_string())
    }
}

impl From<peacock_storage::StorageError> for ApiError {
    fn from(err: peacock_storage::StorageError) -> Self {
        use peacock_storage::StorageError;
        
        match err {
            StorageError::Domain(domain_err) => {
                // Domain errors already have proper classifications
                Self::from(domain_err)
            }
            StorageError::Retryable { message, .. } => {
                Self::conflict(message)
            }
            StorageError::Constraint { table, constraint, message } => {
                Self::conflict(format!("constraint {} on {} violated: {}", constraint, table, message))
            }
            StorageError::Sqlx(sqlx_err) => {
                tracing::error!(target: "peacock_api", error = %sqlx_err, "database error");
                Self::internal("database error")
            }
            StorageError::Connect { .. } => {
                Self::internal("database connection failed")
            }
            other => {
                tracing::error!(target: "peacock_api", error = %other, "storage error");
                Self::internal("storage error")
            }
        }
    }
}

/// RFC 7807 Problem Details payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProblemDetails {
    /// Absolute URI identifying the error class.
    #[serde(rename = "type")]
    pub type_uri: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    /// Request path the failure refers to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Correlates the response with server logs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl ProblemDetails {
    pub fn new(kind: ProblemKind, detail: impl Into<String>, base_uri: &str) -> Self {
        Self {
            type_uri: format!("{}/{}", base_uri.trim_end_matches('/'), kind.slug()),
            title: kind.title().to_string(),
            status: kind.status().as_u16(),
            detail: detail.into(),
            instance: None,
            request_id: None,
        }
    }

    pub fn from_error(err: &ApiError, base_uri: &str) -> Self {
        Self::new(err.kind(), err.detail(), base_uri)
    }

    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Serialises to a `application/problem+json` response.
    pub fn into_response_with_status(self) -> Response {
        let status =
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::to_vec(&self).unwrap_or_else(|_| {
            // Serialising a struct of owned Strings cannot fail; the fallback keeps the
            // contract (valid problem+json) rather than panicking in a handler.
            br#"{"type":"about:blank","title":"Internal Server Error","status":500,"detail":"failed to serialise problem details"}"#
                .to_vec()
        });

        let mut response = (status, body).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PROBLEM_JSON),
        );
        response
    }
}

impl IntoResponse for ApiError {
    /// Produces a complete problem+json response and stashes `self` in the response
    /// extensions.
    ///
    /// The stashed copy lets [`crate::middleware::error`] re-render the body once the
    /// request path and request id are known. Rendering here as well means a handler
    /// tested in isolation still returns a valid document.
    fn into_response(self) -> Response {
        let mut response =
            ProblemDetails::from_error(&self, DEFAULT_PROBLEM_BASE_URI).into_response_with_status();
        response.extensions_mut().insert(self);
        response
    }
}

/// Used only when no configured base URI is reachable (direct `into_response` calls in
/// unit tests). The middleware overwrites it with the configured value.
pub(crate) const DEFAULT_PROBLEM_BASE_URI: &str = "https://peacock-pos.example.com/errors";

/// Convenience alias for handlers.
pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use peacock_core::ids::{BomName, TableName};

    #[test]
    fn domain_not_found_maps_to_404() {
        let err: ApiError = DomainError::TableNotFound(TableName::new("T-001")).into();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.kind(), ProblemKind::NotFound);
        assert!(err.detail().contains("T-001"));

        let menu: ApiError = DomainError::NoActiveMenu.into();
        assert_eq!(menu.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn domain_already_merged_maps_to_409() {
        let err: ApiError = DomainError::AlreadyMerged(TableName::new("T-002")).into();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(err.kind(), ProblemKind::AlreadyExists);
    }

    #[test]
    fn domain_conflicts_map_to_409() {
        for domain in [
            DomainError::TableOccupied(TableName::new("T-003")),
            DomainError::MultipleActiveOrders { count: 2 },
            DomainError::Conflict {
                expected: "1".into(),
                actual: "2".into(),
            },
        ] {
            let err: ApiError = domain.into();
            assert_eq!(err.status(), StatusCode::CONFLICT);
        }
    }

    #[test]
    fn domain_cross_room_merge_maps_to_400() {
        let err: ApiError = DomainError::CrossRoomMerge {
            seed: TableName::new("T-001"),
            target: TableName::new("T-009"),
        }
        .into();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.kind(), ProblemKind::InvalidInput);
    }

    #[test]
    fn data_and_config_faults_map_to_500() {
        for domain in [
            DomainError::BomZeroQuantity(BomName::new("BOM-0001")),
            DomainError::SeriesNotConfigured("ACC-PSINV-".into(), "2025-2026".into()),
            DomainError::InvoiceNameTooLong {
                name: "x".repeat(20),
                limit: 16,
            },
            DomainError::NonNumericData {
                entity: "Item".into(),
                field: "rate".into(),
                raw: "abc".into(),
            },
        ] {
            let err: ApiError = domain.into();
            assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(err.kind(), ProblemKind::Internal);
        }
    }

    #[test]
    fn unauthorized_is_an_http_only_class() {
        let err = ApiError::unauthorized("missing bearer token");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.kind().slug(), "unauthorized");
    }

    #[test]
    fn problem_type_uri_joins_base_and_slug_without_double_slash() {
        let problem = ProblemDetails::new(
            ProblemKind::NotFound,
            "Table 'T-001' does not exist",
            "https://peacock-pos.example.com/errors/",
        );
        assert_eq!(
            problem.type_uri,
            "https://peacock-pos.example.com/errors/not-found"
        );
        assert_eq!(problem.title, "Resource Not Found");
        assert_eq!(problem.status, 404);
    }

    #[test]
    fn problem_omits_absent_optional_members() {
        let json = serde_json::to_value(ProblemDetails::new(
            ProblemKind::Internal,
            "boom",
            "https://e.example/errors",
        ))
        .unwrap();
        assert!(json.get("instance").is_none());
        assert!(json.get("request_id").is_none());
    }

    #[test]
    fn status_reverse_mapping_covers_framework_responses() {
        assert_eq!(
            ProblemKind::from_status(StatusCode::NOT_FOUND),
            ProblemKind::NotFound
        );
        assert_eq!(
            ProblemKind::from_status(StatusCode::METHOD_NOT_ALLOWED),
            ProblemKind::InvalidInput
        );
        assert_eq!(
            ProblemKind::from_status(StatusCode::UNAUTHORIZED),
            ProblemKind::Unauthorized
        );
        assert_eq!(
            ProblemKind::from_status(StatusCode::BAD_GATEWAY),
            ProblemKind::Internal
        );
    }
}
