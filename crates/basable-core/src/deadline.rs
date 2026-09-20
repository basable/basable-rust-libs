//! A horizon held against BOTH clocks.
//!
//! The direct comparison uses the monotonic clock — immune to wall-clock
//! steps, but FROZEN across host suspend and VM freeze. The second uses wall
//! time — which advances across suspend but can be stepped. Each fails in the
//! opposite direction, so a live deadline requires both: a false expiry costs
//! one attempt; a false pass would let a suspend-resumed zombie read as valid
//! after its lease was already adopted. This is the local ownership proof of
//! a processing-object claim (`claim.go`'s `requireProof`), made a type.

use std::time::{Duration, Instant, SystemTime};

/// An instant in the future, remembered on both clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline {
    mono: Instant,
    wall: SystemTime,
}

impl Deadline {
    /// The deadline `d` from now, read on both clocks at the same moment.
    pub fn after(d: Duration) -> Deadline {
        let now = Instant::now();
        let wall = SystemTime::now();
        Deadline {
            mono: now + d,
            wall: wall + d,
        }
    }

    /// The monotonic reading.
    pub fn mono(&self) -> Instant {
        self.mono
    }

    /// The wall-clock reading.
    pub fn wall(&self) -> SystemTime {
        self.wall
    }

    /// Whether the deadline is still ahead on BOTH clocks. The moment either
    /// clock reaches it, the deadline has passed.
    pub fn is_live(&self) -> bool {
        Instant::now() < self.mono && SystemTime::now() < self.wall
    }

    /// The opposite of [`Deadline::is_live`].
    pub fn has_passed(&self) -> bool {
        !self.is_live()
    }

    /// The remaining time on the monotonic clock, zero once passed. Callers
    /// that budget work against a deadline use this; the wall reading exists
    /// only for [`Deadline::is_live`]'s second check.
    pub fn remaining(&self) -> Duration {
        self.mono.saturating_duration_since(Instant::now())
    }

    /// The later of two deadlines (a heartbeat extends a claim's proof by
    /// replacing it with a later one; this keeps a caller from moving a
    /// horizon backwards by mistake).
    pub fn max(self, other: Deadline) -> Deadline {
        if other.mono > self.mono { other } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_future_deadline_is_live_and_a_past_one_is_not() {
        let d = Deadline::after(Duration::from_secs(60));
        assert!(d.is_live());
        assert!(!d.has_passed());
        assert!(d.remaining() > Duration::from_secs(59));

        let passed = Deadline::after(Duration::ZERO);
        std::thread::sleep(Duration::from_millis(2));
        assert!(passed.has_passed());
        assert_eq!(passed.remaining(), Duration::ZERO);
    }

    #[test]
    fn a_stepped_wall_clock_fences_even_when_the_monotonic_clock_is_fine() {
        // Construct a deadline whose wall reading is already behind: the
        // monotonic check alone would pass, the two-clock rule refuses it.
        let d = Deadline {
            mono: Instant::now() + Duration::from_secs(60),
            wall: SystemTime::now() - Duration::from_secs(1),
        };
        assert!(d.has_passed());
    }

    #[test]
    fn max_keeps_the_later_horizon() {
        let early = Deadline::after(Duration::from_secs(1));
        let late = Deadline::after(Duration::from_secs(10));
        assert_eq!(early.max(late), late);
        assert_eq!(late.max(early), late);
    }
}
