//! The bridge from the domain's synchronous port traits to `sqlx`'s async API.
//!
//! `peacock_core::ports` is deliberately synchronous (`ports.rs:7-9`), so somewhere a
//! repository has to block on a query. [`block_on`] is that one place.
//!
//! # Why the future must run on the caller's runtime
//!
//! A `PgPool`'s connections are TCP sockets registered with the reactor of the runtime that
//! opened them. A future that awaits such a connection has to be polled by *that* runtime,
//! or the socket is never polled at all and the acquire sits there until the pool's
//! `acquire_timeout` fires. So the tempting fix for the panic described below — hand the
//! future to a private runtime this module owns — does not work: it turns every query into
//! a pool timeout, which is worse than a panic because it looks like a slow database.
//!
//! That leaves `tokio::task::block_in_place`, which parks the current worker and lets a
//! sibling worker take over driving the runtime, including the reactor. It is the only way
//! to block inside a runtime without starving the I/O the blocked call depends on, and it
//! requires the multi-threaded flavour — a current-thread runtime has no sibling worker to
//! hand the driver to, and tokio panics rather than deadlock.
//!
//! # What that means for callers
//!
//! * Production is fine: the API layer's `#[tokio::main]` is multi-threaded by default.
//! * Sync code with no runtime at all is fine: the future is driven on a runtime this
//!   module builds, and the pool's connections get opened on it.
//! * A test that calls a sync port method must be `#[tokio::test(flavor = "multi_thread")]`.
//!   A bare `#[tokio::test]` is current-thread and gets a panic naming the fix.
//!
//! # Prefer prefetching
//!
//! Even done correctly this parks a worker thread per lookup, and COGS calls
//! `BomRepo::find_for_item` once per BOM line. Where the caller knows its item set up front,
//! the snapshot constructors load everything in a bounded number of queries and then serve
//! the same port traits from memory with no blocking at all — see
//! [`super::bom::PgBomRepo::snapshot_for_items`] and
//! [`super::bundle::PgProductBundleRepo::snapshot`].

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::{Builder, Handle, Runtime, RuntimeFlavor};

use crate::error::StorageError;

/// Runtime used only when the caller has none of its own.
///
/// One worker: the caller is blocked on the result anyway, so more would sit idle. Built on
/// first use, so a process that only ever calls the async methods never pays for it.
fn owned() -> Result<&'static Runtime, StorageError> {
    static OWNED: OnceLock<Runtime> = OnceLock::new();
    if let Some(rt) = OWNED.get() {
        return Ok(rt);
    }
    let rt = Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("peacock-storage-sync")
        .enable_all()
        .build()
        .map_err(|e| StorageError::Internal(format!("building the storage sync-bridge runtime: {e}")))?;
    // `set` races are harmless: another thread won, we use theirs.
    let _ = OWNED.set(rt);
    Ok(OWNED.get().expect("runtime just initialized"))
}

/// Run `fut` to completion, blocking the calling thread.
///
/// The future may borrow from the caller, which is what makes
/// `block_on(self.thing_async(arg))` the natural call shape at every port implementation.
///
/// # Errors
///
/// Returns `StorageError::Internal` if called from inside a **current-thread** runtime
/// or if the fallback runtime cannot be built. There is no correct blocking behaviour
/// available on a current-thread runtime: blocking it stalls the reactor the query
/// needs, and moving the query to another runtime leaves its connection unpolled.
/// Annotate the test `#[tokio::test(flavor = "multi_thread")]`, or call the
/// `*_async` method directly.
pub fn block_on<F>(fut: F) -> Result<F::Output, StorageError>
where
    F: Future,
{
    match Handle::try_current() {
        // Inside a runtime: park this worker and let a sibling drive the reactor, so the
        // pool connection this future is waiting on keeps being polled.
        Ok(handle) => {
            if handle.runtime_flavor() == RuntimeFlavor::CurrentThread {
                return Err(StorageError::Internal(
                    "peacock-storage: a synchronous repository method was called from a \
                     current-thread tokio runtime. Blocking here would stall the reactor that \
                     drives the pool connection the query needs, so there is nothing safe to \
                     do. Use #[tokio::test(flavor = \"multi_thread\")], or call the *_async \
                     method instead."
                        .to_string(),
                ));
            }
            Ok(tokio::task::block_in_place(|| handle.block_on(fut)))
        }
        // No ambient runtime: use ours. Connections opened through it are registered with
        // it, so there is no cross-runtime problem to avoid.
        Err(_) => {
            let rt = owned()?;
            Ok(rt.block_on(fut))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn works_outside_any_runtime() {
        assert_eq!(block_on(async { 6 * 7 }).unwrap(), 42);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn works_inside_a_multi_thread_runtime() {
        assert_eq!(block_on(async { 6 * 7 }).unwrap(), 42);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn accepts_a_future_that_borrows_its_caller() {
        // The shape every port wrapper uses: `block_on(self.thing_async(arg))`. A `'static`
        // bound would reject this and force a clone at every call site.
        let owned = vec![1_u32, 2, 3];
        let borrowed: &[u32] = &owned;
        assert_eq!(
            block_on(async { borrowed.iter().sum::<u32>() }).unwrap(),
            6
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nested_calls_do_not_deadlock() {
        // COGS calls find_for_item once per BOM line, each through this bridge, and a
        // bundle line's BOM walk nests one inside another's stack frame.
        let total = block_on(async { block_on(async { 20 }).unwrap() + 22 }).unwrap();
        assert_eq!(total, 42);
    }

    #[test]
    fn current_thread_runtime_returns_error_with_actionable_message() {
        // Previously documented as a panic; now returns `StorageError::Internal` so the
        // handler can map it to a 500 instead of aborting the worker thread. A timeout
        // here would read as "the database is slow", which sends the reader in the wrong
        // direction.
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime")
            .block_on(async { block_on(async { 1 }) });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("current-thread tokio runtime"),
            "error should mention current-thread runtime, got: {err}"
        );
        assert!(matches!(err, StorageError::Internal(_)));
    }
}
