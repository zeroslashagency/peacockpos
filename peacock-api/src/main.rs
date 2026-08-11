//! Server entry point.
//!
//! Reads configuration, installs the tracing subscriber, binds the socket, and serves
//! until SIGINT or SIGTERM. Shutdown is graceful: in-flight requests finish before the
//! process exits, so a rolling deploy does not drop a payment mid-write.
//!
//! Security note: no authentication layer is mounted yet — every endpoint on this
//! server is currently open to any caller that can reach the port. That is Lane 3B+
//! (auth was deferred to Phase 3B by the plan). Do not expose this build to the public
//! internet without an auth layer or an authenticating proxy in front.
//!
//! ## Startup order, and why the database comes before the socket (Lane 4A-1)
//!
//! `DATABASE_URL` is read and the pool is built, verified and migrated *before* the
//! listener binds. A process that accepted connections first would spend its first
//! seconds answering every request with a 500, and a readiness probe that saw the open
//! port would route live traffic into it. Failing before the bind means an orchestrator
//! sees a process that never became ready, which is the accurate signal.
//!
//! There is no in-memory fallback here, and as of Lane W1-A there is none anywhere else
//! either: `AppState` cannot be constructed without a `Storage`, so this is the only
//! startup path there is. A POS that silently kept orders in a HashMap because the
//! database was unreachable would take a shift's takings and lose them at the next
//! restart, so a missing `DATABASE_URL` is a startup failure with an actionable message
//! rather than a degraded mode.

use std::process::ExitCode;

use tokio::net::TcpListener;
use tokio::signal;

use peacock_api::config::Config;
use peacock_api::middleware::logging::init_tracing;
use peacock_storage::{DbConfig, Storage};

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(message) => {
            // Tracing is not up yet, so this goes to stderr directly.
            eprintln!("configuration error: {message}");
            return ExitCode::FAILURE;
        }
    };

    init_tracing(config.log_format);

    // ---------------------------------------------------------------------
    // Database: connect, verify, migrate. Before the socket, deliberately.
    // ---------------------------------------------------------------------
    let db_config = match DbConfig::from_env() {
        Ok(db_config) => db_config,
        Err(err) => {
            tracing::error!(
                error = %err,
                "DATABASE_URL is not set or is blank, and peacock-api has no in-memory \
                 mode to fall back to. Set it and start again, e.g.\n  \
                 DATABASE_URL=postgres://localhost:5432/peacock\n\
                 See .env.example for the optional pool settings."
            );
            // Also to stderr: an operator running this by hand should not have to know
            // the log format to find out why the process would not start.
            eprintln!(
                "peacock-api: cannot start without a database. {err}\n  \
                 set DATABASE_URL, e.g. DATABASE_URL=postgres://localhost:5432/peacock"
            );
            return ExitCode::FAILURE;
        }
    };

    // `Storage::connect` eagerly acquires a connection, runs the health check and applies
    // pending migrations, so a bad URL, an unreachable server or a failed migration all
    // surface here rather than inside the first request.
    let storage = match Storage::connect(db_config).await {
        Ok(storage) => storage,
        Err(err) => {
            tracing::error!(
                error = %err,
                "could not connect to the database, verify it, or apply migrations; \
                 refusing to serve"
            );
            eprintln!("peacock-api: database unavailable. {err}");
            return ExitCode::FAILURE;
        }
    };

    let bind_addr = config.bind_addr;
    let origins = config.cors_allowed_origins.clone();
    let db_url = storage.config().redacted_url();
    let app = peacock_api::build_with_storage(config, storage.clone());

    let listener = match TcpListener::bind(bind_addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(addr = %bind_addr, error = %err, "failed to bind");
            // The pool holds live sockets; close it so the server does not leave
            // connections behind on a failed start.
            storage.close().await;
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(
        addr = %bind_addr,
        database = %db_url,
        cors_origins = ?origins,
        "peacock-api listening (no authentication layer mounted yet)"
    );

    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    // After the in-flight requests have drained, not before: closing the pool while a
    // payment is still writing would abort it mid-transaction.
    storage.close().await;

    if let Err(err) = serve_result {
        tracing::error!(error = %err, "server error");
        return ExitCode::FAILURE;
    }

    tracing::info!("shutdown complete");
    ExitCode::SUCCESS
}

/// Resolves on SIGINT (Ctrl-C) or SIGTERM (container stop).
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = signal::ctrl_c().await {
            tracing::error!(error = %err, "failed to install SIGINT handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(err) => tracing::error!(error = %err, "failed to install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT, draining"),
        _ = terminate => tracing::info!("received SIGTERM, draining"),
    }
}
