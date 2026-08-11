//! End-to-end tests against a real bound socket.
//!
//! The unit tests in `src/app.rs` drive the router with `oneshot`, which skips the
//! listener and the HTTP wire format. These boot the server on an ephemeral port and
//! talk to it over TCP, so a regression in binding, serving, or graceful shutdown is
//! caught too.
//!
//! Lane W1-A: these used to call `peacock_api::build(config)`, which needed no database.
//! That function is gone, so each server here is booted over a throwaway PostgreSQL
//! database via `support::TestApp` — the same harness the other integration tests use. The
//! endpoints exercised (`/health`, an unknown path) do not touch a table, but the server
//! cannot be constructed without a pool, which is the point of the lane.

mod support;

use std::net::SocketAddr;

use support::TestApp;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Boots the app on port 0 and returns its address, a shutdown trigger, and the database.
///
/// The `TestApp` comes back so the caller can hold it: dropping it drops the database out
/// from under the running server.
async fn spawn_server() -> (SocketAddr, oneshot::Sender<()>, TestApp) {
    let harness = TestApp::new().await;

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("binding an ephemeral port must succeed");
    let addr = listener.local_addr().expect("listener has a local address");

    let app = harness.app.clone();
    let (tx, rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                // Dropping the sender also shuts the server down.
                let _ = rx.await;
            })
            .await
            .expect("server runs until shutdown");
    });

    (addr, tx, harness)
}

#[tokio::test]
async fn health_endpoint_answers_over_tcp() {
    let (addr, _shutdown, _db) = spawn_server().await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .expect("server accepts connections");

    assert_eq!(response.status().as_u16(), 200);

    let request_id = response
        .headers()
        .get("x-request-id")
        .expect("x-request-id header present")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        uuid::Uuid::parse_str(&request_id).is_ok(),
        "request id {request_id} must be a UUID"
    );

    let body: serde_json::Value = response.json().await.expect("body is JSON");
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn unknown_path_returns_problem_json_over_tcp() {
    let (addr, _shutdown, _db) = spawn_server().await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/api/missing"))
        .header("x-request-id", "e2e-trace-1")
        .send()
        .await
        .expect("request completes");

    assert_eq!(response.status().as_u16(), 404);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "application/problem+json"
    );
    assert_eq!(response.headers().get("x-request-id").unwrap(), "e2e-trace-1");

    let body: serde_json::Value = response.json().await.expect("body is JSON");
    assert_eq!(
        body["type"],
        "https://peacock-pos.example.com/errors/not-found"
    );
    assert_eq!(body["status"], 404);
    assert_eq!(body["instance"], "/api/missing");
    assert_eq!(body["request_id"], "e2e-trace-1");
}

#[tokio::test]
async fn graceful_shutdown_stops_accepting_connections() {
    let (addr, shutdown, _db) = spawn_server().await;

    let client = reqwest::Client::new();
    let before = client
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .expect("server is up");
    assert_eq!(before.status().as_u16(), 200);

    shutdown.send(()).expect("shutdown signal delivered");

    // Give the accept loop a moment to wind down, then confirm the port is closed.
    let mut refused = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        if tokio::net::TcpStream::connect(addr).await.is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused, "port must stop accepting after graceful shutdown");
}
