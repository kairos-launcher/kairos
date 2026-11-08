//! While the user drags the window, the compositor may take keyboard focus away (GNOME does as
//! soon as the move starts). That must not be mistaken for "clicked elsewhere", which hides
//! the launcher.

use std::time::{Duration, Instant};

/// A drag is assumed finished after this long, so a missed "drag ended" signal can never make
/// the launcher ignore a real click-away forever.
const MAX_DRAG: Duration = Duration::from_secs(20);

#[derive(Default)]
pub struct DragGuard {
    started: Option<Instant>,
}

impl DragGuard {
    pub fn start(&mut self, now: Instant) {
        self.started = Some(now);
    }

    pub fn end(&mut self) {
        self.started = None;
    }

    /// Is a drag (that we started) still plausibly in progress?
    pub fn active(&self, now: Instant) -> bool {
        self.started.is_some_and(|t| now.saturating_duration_since(t) < MAX_DRAG)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_is_ignored_only_while_a_drag_is_active() {
        let mut g = DragGuard::default();
        let t0 = Instant::now();
        assert!(!g.active(t0), "no drag: a blur is a real click-away");
        g.start(t0);
        assert!(g.active(t0 + Duration::from_secs(5)), "a long drag is still a drag");
        g.end();
        assert!(!g.active(t0 + Duration::from_secs(5)), "ended explicitly");
    }

    #[test]
    fn a_stuck_drag_expires() {
        let mut g = DragGuard::default();
        let t0 = Instant::now();
        g.start(t0);
        assert!(!g.active(t0 + MAX_DRAG), "never ignore click-away forever");
    }
}
