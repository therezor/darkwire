//! Synchronous store work, run off the async runtime.
//!
//! Every store here is SQLite behind one shared lock. A call made on a runtime
//! worker holds that worker for as long as the lock is contended, and the same
//! worker is driving other sessions' token streams. The hub and the routes
//! reach the stores through this one function for that reason.

use darkwire_core::{ErrorKind, Result, WireError};

/// Runs `work` on the blocking pool and waits for it.
///
/// A task that panicked or was cancelled is the one generic internal error:
/// neither is something a caller can act on.
pub async fn blocking<T, F>(work: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| {
            Err(WireError::new(
                ErrorKind::Internal,
                format!("A store call did not finish: {error}"),
            ))
        })
}
