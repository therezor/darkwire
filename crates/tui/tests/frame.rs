//! Collecting many reasons to redraw into one draw.

use std::time::Duration;

use darkwire_tui::frame::{FRAME_INTERVAL, scheduler};
use tokio::time;

/// Whether a draw arrives within `window` of simulated time.
async fn drawn_within(draws: &mut tokio::sync::broadcast::Receiver<()>, window: Duration) -> bool {
    time::timeout(window, draws.recv()).await.is_ok()
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_request_becomes_a_draw() {
    let (requester, draws) = scheduler();
    let mut draws = draws.subscribe();

    requester.schedule_frame();

    assert!(drawn_within(&mut draws, Duration::from_millis(100)).await);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_burst_of_requests_becomes_one_draw() {
    let (requester, draws) = scheduler();
    let mut draws = draws.subscribe();

    for _ in 0..20 {
        requester.schedule_frame();
    }

    assert!(drawn_within(&mut draws, Duration::from_millis(100)).await);
    assert!(
        !drawn_within(&mut draws, FRAME_INTERVAL / 2).await,
        "a second draw for the same burst"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn nothing_is_drawn_before_it_was_asked_for() {
    let (requester, draws) = scheduler();
    let mut draws = draws.subscribe();

    requester.schedule_frame_in(Duration::from_millis(200));

    assert!(
        !drawn_within(&mut draws, Duration::from_millis(100)).await,
        "the draw fired early"
    );
    assert!(drawn_within(&mut draws, Duration::from_millis(200)).await);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn two_draws_are_never_closer_than_a_frame() {
    let (requester, draws) = scheduler();
    let mut draws = draws.subscribe();

    requester.schedule_frame();
    assert!(drawn_within(&mut draws, Duration::from_millis(100)).await);

    // A turn producing text asks again immediately. It still waits.
    requester.schedule_frame();
    assert!(
        !drawn_within(&mut draws, FRAME_INTERVAL / 2).await,
        "two draws inside one frame interval"
    );
    assert!(drawn_within(&mut draws, FRAME_INTERVAL).await);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn nothing_is_drawn_when_nothing_asked() {
    let (_requester, draws) = scheduler();
    let mut draws = draws.subscribe();

    assert!(!drawn_within(&mut draws, Duration::from_secs(2)).await);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_detached_requester_asks_nobody() {
    let requester = darkwire_tui::frame::FrameRequester::detached();
    // Nothing is listening, so this is a no-op rather than a panic.
    requester.schedule_frame();
    requester.schedule_frame_in(Duration::from_millis(10));
}
