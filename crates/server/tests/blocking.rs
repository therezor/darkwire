//! The one way the hub and the routes reach a store from async code.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_core::{ErrorKind, WireError};
use darkwire_server::blocking::blocking;

#[tokio::test]
async fn hands_back_what_the_work_returned() {
    assert_eq!(blocking(|| Ok(7)).await.unwrap(), 7);
    let error = blocking::<(), _>(|| Err(WireError::new(ErrorKind::NotFound, "gone")))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
}

#[tokio::test]
async fn runs_off_the_runtime_thread() {
    let runtime_thread = std::thread::current().id();
    let worker = blocking(|| Ok(std::thread::current().id())).await.unwrap();
    assert_ne!(worker, runtime_thread);
}

#[tokio::test]
async fn a_panic_in_the_work_is_an_internal_error_not_a_crash() {
    let error = blocking::<(), _>(|| panic!("the store fell over"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Internal);
}
