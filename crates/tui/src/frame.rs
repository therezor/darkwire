//! Deciding when to draw, once, for everything that wants to.
//!
//! A turn produces text faster than a screen can usefully show it: a hundred
//! deltas a second, each one a reason to redraw. Drawing on each is wasted
//! work and, worse, a terminal that never catches up. Drawing on a timer
//! instead means a keystroke waits for the tick.
//!
//! So a request is not a draw. Anything that changed what the screen should
//! say calls [`FrameRequester::schedule_frame`], and a scheduler task collects
//! those into one notification no more often than [`FRAME_INTERVAL`]. The loop
//! sees a single "draw now" and draws the current state, not a backlog of the
//! states it passed through.
//!
//! The spinner is why [`FrameRequester::schedule_frame_in`] exists: nothing
//! changed, but something should be different in eighty milliseconds.

use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};

/// The shortest gap between two draws: about thirty frames a second.
///
/// Faster than a reader can follow and slower than a terminal can choke on.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// A handle anything can hold to ask for a draw.
#[derive(Clone, Debug)]
pub struct FrameRequester {
    deadlines: mpsc::UnboundedSender<Instant>,
}

impl FrameRequester {
    /// Asks for a draw as soon as the frame interval allows.
    pub fn schedule_frame(&self) {
        let _ = self.deadlines.send(Instant::now());
    }

    /// Asks for a draw once `after` has passed.
    pub fn schedule_frame_in(&self, after: Duration) {
        let _ = self.deadlines.send(Instant::now() + after);
    }

    /// A requester nothing is listening to, for a test that does not draw.
    #[must_use]
    pub fn detached() -> Self {
        let (deadlines, _) = mpsc::unbounded_channel();
        Self { deadlines }
    }
}

/// Starts the scheduler and returns the handle and the draw notifications.
///
/// The receiver end is a broadcast so a test can subscribe beside the loop.
/// One slot is enough: a second notification before the first was read means
/// the same thing as the first, which is "the screen is out of date".
#[must_use]
pub fn scheduler() -> (FrameRequester, broadcast::Sender<()>) {
    let (deadlines, requests) = mpsc::unbounded_channel();
    let (draws, _) = broadcast::channel(1);
    tokio::spawn(run(requests, draws.clone()));
    (FrameRequester { deadlines }, draws)
}

/// Collects requests and emits at most one draw per frame interval.
async fn run(mut requests: mpsc::UnboundedReceiver<Instant>, draws: broadcast::Sender<()>) {
    // Long enough that the sleep never fires on its own, short enough to be a
    // duration rather than a special case in the branch below.
    const FOREVER: Duration = Duration::from_hours(24 * 365);
    let mut next: Option<Instant> = None;
    let mut last_drawn: Option<Instant> = None;

    loop {
        let target = next.unwrap_or_else(|| Instant::now() + FOREVER);
        let sleep = tokio::time::sleep_until(target.into());
        tokio::pin!(sleep);

        tokio::select! {
            request = requests.recv() => {
                let Some(request) = request else {
                    // Every handle is gone, so nothing can ask again.
                    break;
                };
                let earliest = last_drawn.map_or(request, |last| request.max(last + FRAME_INTERVAL));
                next = Some(next.map_or(earliest, |current| current.min(earliest)));
                // Deliberately not drawing here. Going round again recomputes
                // the sleep, which is what folds a burst of requests into one.
            }
            () = &mut sleep => {
                if next.take().is_some() {
                    last_drawn = Some(Instant::now());
                    let _ = draws.send(());
                }
            }
        }
    }
}
