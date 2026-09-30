//! Host-adapter bridge tests (lap-517 / TASK-205, engine issue rapidraw-d73):
//! the Tauri host owns legacy `(Arc<AtomicUsize>, usize)` cancellation
//! generation pairs shared across its loader workers. The engine crate must
//! let a typed host adapter bind such an external pair to a
//! [`rapidraw_develop::CancelToken`] without wrapping or replacing the host's
//! shared tracker, so cancellation keeps its historic semantics.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use rapidraw_develop::{CancelToken, DevelopError};

#[test]
fn cancel_token_binds_to_a_shared_host_tracker() {
    let tracker = Arc::new(AtomicUsize::new(7));
    let token = CancelToken::from_shared_parts(tracker.clone(), 7);
    assert!(
        !token.is_cancelled(),
        "an unbumped tracker at the bound generation must not cancel"
    );
    tracker.fetch_add(1, Ordering::SeqCst);
    assert!(
        token.is_cancelled(),
        "bumping the shared tracker must cancel the bound token"
    );
    assert!(matches!(token.check(), Err(DevelopError::Cancelled)));
}

#[test]
fn cancel_token_rejects_a_stale_generation_binding() {
    let tracker = Arc::new(AtomicUsize::new(3));
    let token = CancelToken::from_shared_parts(tracker.clone(), 2);
    assert!(
        token.is_cancelled(),
        "a generation older than the tracker value is already cancelled"
    );
    assert!(matches!(token.check(), Err(DevelopError::Cancelled)));
}

#[test]
fn cancel_token_survives_cloning_like_the_host_pair() {
    let tracker = Arc::new(AtomicUsize::new(1));
    let token = CancelToken::from_shared_parts(tracker.clone(), 1);
    let cloned = token.clone();
    tracker.fetch_add(1, Ordering::SeqCst);
    assert!(token.is_cancelled() && cloned.is_cancelled());
}
