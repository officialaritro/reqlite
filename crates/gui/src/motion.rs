//! The few animations of a tab, and the clock they read. Frames are drawn only
//! while one of them runs, so an idle window costs no CPU.

use iced::animation::{Animation, Easing};
use std::time::{Duration, Instant};

pub struct Motion {
    pub now: Instant,
    /// The response fading in.
    pub reveal: Animation<bool>,
    /// Send turning into Cancel and back.
    pub running: Animation<bool>,
    /// When the current send started, for the pulse.
    pub since: Instant,
    /// `REQLITE_REDUCE_MOTION=1`: every change shows at once.
    pub reduced: bool,
}

/// The "sending" pulse redraws at most this often.
pub const PULSE: Duration = Duration::from_millis(33);

impl Motion {
    pub fn new(reduced: bool) -> Motion {
        let now = Instant::now();
        Motion {
            now,
            reveal: Animation::new(true),
            running: Animation::new(false)
                .duration(if reduced {
                    Duration::ZERO
                } else {
                    Duration::from_millis(200)
                })
                .easing(Easing::EaseOut),
            since: now,
            reduced,
        }
    }

    /// Fades a new response in from nothing: 0.5 s, rising 4 px (Zeron's fade-in).
    pub fn reveal(&mut self, now: Instant) {
        let fade = Animation::new(false).easing(Easing::EaseOutExpo);
        self.reveal = fade
            .duration(if self.reduced {
                Duration::ZERO
            } else {
                Duration::from_millis(500)
            })
            .go(true, now);
    }

    pub fn animating(&self, at: Instant) -> bool {
        self.reveal.is_animating(at) || self.running.is_animating(at)
    }
}
