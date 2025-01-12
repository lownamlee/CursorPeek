use std::time::{Duration, Instant};

pub(crate) const DEFAULT_DWELL_DELAY: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PhysicalScreenPoint {
    pub(crate) x: i32,
    pub(crate) y: i32,
}

impl PhysicalScreenPoint {
    pub(crate) fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Generation(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DwellTimerEvent {
    Inactive,
    Rearm(Duration),
    Ready {
        generation: Generation,
        point: PhysicalScreenPoint,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingDwell {
    generation: Generation,
    point: PhysicalScreenPoint,
    deadline: Instant,
}

pub(crate) struct HoverState {
    delay: Duration,
    generation: Generation,
    pending: Option<PendingDwell>,
}

impl HoverState {
    pub(crate) fn new(delay: Duration) -> Self {
        debug_assert!(!delay.is_zero());

        Self {
            delay,
            generation: Generation::default(),
            pending: None,
        }
    }

    pub(crate) fn restart(&mut self, point: PhysicalScreenPoint, now: Instant) -> Duration {
        self.advance_generation();
        self.pending = Some(PendingDwell {
            generation: self.generation,
            point,
            deadline: now.checked_add(self.delay).unwrap_or(now),
        });
        self.delay
    }

    pub(crate) fn cancel(&mut self) {
        self.advance_generation();
        self.pending = None;
    }

    pub(crate) fn on_timer(&mut self, now: Instant) -> DwellTimerEvent {
        let Some(pending) = self.pending else {
            return DwellTimerEvent::Inactive;
        };

        if now < pending.deadline {
            return DwellTimerEvent::Rearm(pending.deadline.duration_since(now));
        }

        self.pending = None;
        DwellTimerEvent::Ready {
            generation: pending.generation,
            point: pending.point,
        }
    }

    fn advance_generation(&mut self) {
        self.generation = Generation(self.generation.0.wrapping_add(1));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DwellTimerEvent, Generation, HoverState, PhysicalScreenPoint, DEFAULT_DWELL_DELAY,
    };
    use std::time::{Duration, Instant};

    const FIRST_POINT: PhysicalScreenPoint = PhysicalScreenPoint { x: -120, y: 45 };
    const SECOND_POINT: PhysicalScreenPoint = PhysicalScreenPoint { x: 980, y: -30 };

    #[test]
    fn new_state_has_no_pending_dwell() {
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);

        assert_eq!(state.on_timer(Instant::now()), DwellTimerEvent::Inactive);
    }

    #[test]
    fn activity_rearms_until_the_monotonic_deadline() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);

        assert_eq!(state.restart(FIRST_POINT, start), DEFAULT_DWELL_DELAY);
        assert_eq!(
            state.on_timer(start + Duration::from_millis(125)),
            DwellTimerEvent::Rearm(Duration::from_millis(275))
        );
        assert_eq!(
            state.on_timer(start + DEFAULT_DWELL_DELAY),
            DwellTimerEvent::Ready {
                generation: Generation(1),
                point: FIRST_POINT,
            }
        );
        assert_eq!(
            state.on_timer(start + DEFAULT_DWELL_DELAY),
            DwellTimerEvent::Inactive
        );
    }

    #[test]
    fn repeated_activity_replaces_the_pending_point_and_deadline() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);

        state.restart(FIRST_POINT, start);
        state.restart(SECOND_POINT, start + Duration::from_millis(300));

        assert_eq!(
            state.on_timer(start + DEFAULT_DWELL_DELAY),
            DwellTimerEvent::Rearm(Duration::from_millis(300))
        );
        assert_eq!(
            state.on_timer(start + Duration::from_millis(700)),
            DwellTimerEvent::Ready {
                generation: Generation(2),
                point: SECOND_POINT,
            }
        );
    }

    #[test]
    fn cancellation_invalidates_pending_work() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);

        state.restart(FIRST_POINT, start);
        state.cancel();

        assert_eq!(
            state.on_timer(start + DEFAULT_DWELL_DELAY),
            DwellTimerEvent::Inactive
        );
    }

    #[test]
    fn generation_wraps_without_panicking() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);
        state.generation = Generation(u64::MAX);

        state.restart(FIRST_POINT, start);

        assert_eq!(
            state.on_timer(start + DEFAULT_DWELL_DELAY),
            DwellTimerEvent::Ready {
                generation: Generation(0),
                point: FIRST_POINT,
            }
        );
    }
}
