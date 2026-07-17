use std::time::{Duration, Instant};

pub(crate) const DEFAULT_DWELL_DELAY: Duration = Duration::from_millis(400);
const BASE_DPI: u32 = 96;
const MIN_HOVER_DIMENSION: u32 = 4;

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

impl Generation {
    pub(crate) const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HoverRectangle {
    width: u32,
    height: u32,
}

impl HoverRectangle {
    pub(crate) fn from_96_dpi(width: u32, height: u32, target_dpi: u32) -> Option<Self> {
        if target_dpi == 0 {
            return None;
        }

        Some(Self {
            width: scale_up_from_96_dpi(width, target_dpi).max(MIN_HOVER_DIMENSION),
            height: scale_up_from_96_dpi(height, target_dpi).max(MIN_HOVER_DIMENSION),
        })
    }

    pub(crate) fn contains(self, anchor: PhysicalScreenPoint, point: PhysicalScreenPoint) -> bool {
        contains_axis(anchor.x, point.x, self.width)
            && contains_axis(anchor.y, point.y, self.height)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DwellCandidate {
    generation: Generation,
    anchor: PhysicalScreenPoint,
}

impl DwellCandidate {
    pub(crate) fn anchor(self) -> PhysicalScreenPoint {
        self.anchor
    }

    pub(crate) fn validate(
        self,
        current: PhysicalScreenPoint,
        hover_rectangle: HoverRectangle,
    ) -> Option<ReadyDwell> {
        hover_rectangle
            .contains(self.anchor, current)
            .then_some(ReadyDwell {
                generation: self.generation,
                point: current,
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReadyDwell {
    generation: Generation,
    point: PhysicalScreenPoint,
}

impl ReadyDwell {
    pub(crate) fn into_parts(self) -> (Generation, PhysicalScreenPoint) {
        (self.generation, self.point)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DwellTimerEvent {
    Inactive,
    Rearm(Duration),
    Candidate(DwellCandidate),
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
        DwellTimerEvent::Candidate(DwellCandidate {
            generation: pending.generation,
            anchor: pending.point,
        })
    }

    fn advance_generation(&mut self) {
        self.generation = Generation(self.generation.0.wrapping_add(1));
    }
}

fn scale_up_from_96_dpi(value: u32, target_dpi: u32) -> u32 {
    let scaled = u64::from(value)
        .saturating_mul(u64::from(target_dpi))
        .div_ceil(u64::from(BASE_DPI));

    scaled.min(u64::from(u32::MAX)) as u32
}

fn contains_axis(anchor: i32, point: i32, extent: u32) -> bool {
    let extent = i64::from(extent);
    let start = i64::from(anchor) - extent / 2;
    let end = start + extent;
    let point = i64::from(point);

    start <= point && point < end
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_DWELL_DELAY, DwellCandidate, DwellTimerEvent, Generation, HoverRectangle,
        HoverState, PhysicalScreenPoint, ReadyDwell,
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
            DwellTimerEvent::Candidate(DwellCandidate {
                generation: Generation(1),
                anchor: FIRST_POINT,
            })
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
            DwellTimerEvent::Candidate(DwellCandidate {
                generation: Generation(2),
                anchor: SECOND_POINT,
            })
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
            DwellTimerEvent::Candidate(DwellCandidate {
                generation: Generation(0),
                anchor: FIRST_POINT,
            })
        );
    }

    #[test]
    fn hover_rectangle_scales_up_and_keeps_a_physical_minimum() {
        assert_eq!(
            HoverRectangle::from_96_dpi(1, 3, 96),
            Some(HoverRectangle {
                width: 4,
                height: 4,
            })
        );
        assert_eq!(
            HoverRectangle::from_96_dpi(4, 5, 120),
            Some(HoverRectangle {
                width: 5,
                height: 7,
            })
        );
        assert_eq!(HoverRectangle::from_96_dpi(4, 4, 0), None);
    }

    #[test]
    fn hover_rectangle_uses_half_open_bounds_on_negative_desktops() {
        let rectangle = HoverRectangle::from_96_dpi(4, 5, 96).unwrap();
        let anchor = PhysicalScreenPoint::new(-100, -50);

        for point in [
            PhysicalScreenPoint::new(-102, -52),
            PhysicalScreenPoint::new(-99, -48),
            anchor,
        ] {
            assert!(rectangle.contains(anchor, point));
        }

        for point in [
            PhysicalScreenPoint::new(-103, -50),
            PhysicalScreenPoint::new(-98, -50),
            PhysicalScreenPoint::new(-100, -53),
            PhysicalScreenPoint::new(-100, -47),
        ] {
            assert!(!rectangle.contains(anchor, point));
        }
    }

    #[test]
    fn hover_rectangle_math_handles_coordinate_extremes() {
        let rectangle = HoverRectangle::from_96_dpi(u32::MAX, u32::MAX, u32::MAX).unwrap();

        assert!(rectangle.contains(
            PhysicalScreenPoint::new(i32::MIN, i32::MAX),
            PhysicalScreenPoint::new(i32::MIN, i32::MAX),
        ));
        assert!(rectangle.contains(
            PhysicalScreenPoint::new(i32::MAX, i32::MIN),
            PhysicalScreenPoint::new(i32::MAX, i32::MIN),
        ));
    }

    #[test]
    fn candidate_validation_emits_only_the_current_in_bounds_point() {
        let candidate = DwellCandidate {
            generation: Generation(7),
            anchor: FIRST_POINT,
        };
        let rectangle = HoverRectangle::from_96_dpi(4, 4, 96).unwrap();
        let current = PhysicalScreenPoint::new(FIRST_POINT.x + 1, FIRST_POINT.y - 2);

        assert_eq!(
            candidate.validate(current, rectangle),
            Some(ReadyDwell {
                generation: Generation(7),
                point: current,
            })
        );
        assert_eq!(
            candidate.validate(
                PhysicalScreenPoint::new(FIRST_POINT.x + 2, FIRST_POINT.y),
                rectangle,
            ),
            None
        );
    }
}
