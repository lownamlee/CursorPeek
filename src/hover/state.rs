use std::time::{Duration, Instant};

use cursorpeek_core::{PhysicalScreenRect, PhysicalScreenSpan};

use super::{Generation, PhysicalScreenPoint};

pub(crate) const DEFAULT_DWELL_DELAY: Duration = Duration::from_millis(250);
pub(crate) const PREVIEW_RESHOW_DELAY: Duration = Duration::from_millis(50);
pub(crate) const PREVIEW_RESHOW_GRACE: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DwellCandidate {
    generation: Generation,
    anchor: PhysicalScreenPoint,
    point: PhysicalScreenPoint,
    pointer_span: PhysicalScreenSpan,
}

impl DwellCandidate {
    pub(crate) fn include(mut self, point: PhysicalScreenPoint) -> Self {
        self.point = point;
        self.pointer_span.include(point);
        self
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Generation,
        PhysicalScreenPoint,
        PhysicalScreenPoint,
        PhysicalScreenSpan,
    ) {
        (self.generation, self.anchor, self.point, self.pointer_span)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DwellTimerEvent {
    Inactive,
    Rearm(Duration),
    Candidate(DwellCandidate),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TrackedDwell {
    generation: Generation,
    anchor: PhysicalScreenPoint,
    point: PhysicalScreenPoint,
    pointer_span: PhysicalScreenSpan,
    deadline: Option<Instant>,
}

pub(crate) struct HoverState {
    delay: Duration,
    generation: Generation,
    tracked: Option<TrackedDwell>,
    reshow_until: Option<Instant>,
}

impl HoverState {
    pub(crate) fn new(delay: Duration) -> Self {
        debug_assert!(!delay.is_zero());

        Self {
            delay,
            generation: Generation::default(),
            tracked: None,
            reshow_until: None,
        }
    }

    pub(crate) fn restart(&mut self, point: PhysicalScreenPoint, now: Instant) -> Duration {
        let delay = self.current_delay(now);
        self.schedule(point, now, delay)
    }

    pub(crate) fn restart_after_preview(
        &mut self,
        point: PhysicalScreenPoint,
        now: Instant,
    ) -> Duration {
        self.reshow_until = now.checked_add(PREVIEW_RESHOW_GRACE);
        let delay = self.current_delay(now);
        self.schedule(point, now, delay)
    }

    fn schedule(&mut self, point: PhysicalScreenPoint, now: Instant, delay: Duration) -> Duration {
        self.advance_generation();
        self.tracked = Some(TrackedDwell {
            generation: self.generation,
            anchor: point,
            point,
            pointer_span: PhysicalScreenSpan::from_point(point),
            deadline: Some(now.checked_add(delay).unwrap_or(now)),
        });
        delay
    }

    pub(crate) fn track_motion(&mut self, point: PhysicalScreenPoint) -> bool {
        let Some(tracked) = self.tracked.as_mut() else {
            return false;
        };
        tracked.point = point;
        tracked.pointer_span.include(point);
        true
    }

    pub(crate) fn tracking_anchor(&self) -> Option<PhysicalScreenPoint> {
        self.tracked.map(|tracked| tracked.anchor)
    }

    pub(crate) fn tracked_span_fits(
        &self,
        generation: Generation,
        target_bounds: PhysicalScreenRect,
    ) -> bool {
        self.tracked.is_some_and(|tracked| {
            tracked.generation == generation && tracked.pointer_span.fits_within(target_bounds)
        })
    }

    pub(crate) fn finish_resolution(&mut self, generation: Generation) {
        if self
            .tracked
            .is_some_and(|tracked| tracked.generation == generation)
        {
            self.tracked = None;
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.advance_generation();
        self.tracked = None;
        self.reshow_until = None;
    }

    pub(crate) fn preview_shown(&mut self) {
        self.tracked = None;
        self.reshow_until = None;
    }

    pub(crate) fn set_delay(&mut self, delay: Duration) {
        debug_assert!(!delay.is_zero());
        self.delay = delay;
    }

    #[cfg(test)]
    pub(crate) const fn delay(&self) -> Duration {
        self.delay
    }

    pub(crate) const fn generation(&self) -> Generation {
        self.generation
    }

    pub(crate) fn on_timer(&mut self, now: Instant) -> DwellTimerEvent {
        let Some(tracked) = self.tracked.as_mut() else {
            return DwellTimerEvent::Inactive;
        };
        let Some(deadline) = tracked.deadline else {
            return DwellTimerEvent::Inactive;
        };

        if now < deadline {
            return DwellTimerEvent::Rearm(deadline.duration_since(now));
        }

        tracked.deadline = None;
        DwellTimerEvent::Candidate(DwellCandidate {
            generation: tracked.generation,
            anchor: tracked.anchor,
            point: tracked.point,
            pointer_span: tracked.pointer_span,
        })
    }

    fn advance_generation(&mut self) {
        self.generation = Generation::from_raw(self.generation.get().wrapping_add(1));
    }

    fn current_delay(&mut self, now: Instant) -> Duration {
        if self.reshow_until.is_some_and(|deadline| now < deadline) {
            self.delay.min(PREVIEW_RESHOW_DELAY)
        } else {
            self.reshow_until = None;
            self.delay
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_DWELL_DELAY, DwellCandidate, DwellTimerEvent, Generation, HoverState,
        PREVIEW_RESHOW_DELAY, PREVIEW_RESHOW_GRACE, PhysicalScreenPoint, PhysicalScreenRect,
        PhysicalScreenSpan,
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
    fn motion_updates_the_pointer_span_without_postponing_the_deadline() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);

        assert_eq!(state.restart(FIRST_POINT, start), DEFAULT_DWELL_DELAY);
        assert_eq!(state.tracking_anchor(), Some(FIRST_POINT));
        assert!(state.track_motion(PhysicalScreenPoint::new(-100, 50)));
        assert_eq!(
            state.on_timer(start + Duration::from_millis(125)),
            DwellTimerEvent::Rearm(Duration::from_millis(125))
        );
        assert_eq!(
            state.on_timer(start + DEFAULT_DWELL_DELAY),
            DwellTimerEvent::Candidate(DwellCandidate {
                generation: Generation::from_raw(1),
                anchor: FIRST_POINT,
                point: PhysicalScreenPoint::new(-100, 50),
                pointer_span: PhysicalScreenSpan::try_new(-120, 45, -100, 50).unwrap(),
            })
        );
        assert_eq!(
            state.on_timer(start + DEFAULT_DWELL_DELAY),
            DwellTimerEvent::Inactive
        );
    }

    #[test]
    fn an_explicit_restart_replaces_the_tracked_item_and_deadline() {
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
                generation: Generation::from_raw(2),
                anchor: SECOND_POINT,
                point: SECOND_POINT,
                pointer_span: PhysicalScreenSpan::from_point(SECOND_POINT),
            })
        );
    }

    #[test]
    fn changing_delay_invalidates_pending_work_and_applies_to_the_next_dwell() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);
        state.restart(FIRST_POINT, start);

        let replacement = Duration::from_millis(700);
        state.cancel();
        state.set_delay(replacement);
        assert_eq!(
            state.on_timer(start + DEFAULT_DWELL_DELAY),
            DwellTimerEvent::Inactive
        );
        assert_eq!(state.restart(SECOND_POINT, start), replacement);
        assert_eq!(
            state.on_timer(start + replacement),
            DwellTimerEvent::Candidate(DwellCandidate {
                generation: Generation::from_raw(3),
                anchor: SECOND_POINT,
                point: SECOND_POINT,
                pointer_span: PhysicalScreenSpan::from_point(SECOND_POINT),
            })
        );
    }

    #[test]
    fn cancellation_invalidates_pending_work() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);

        state.restart(FIRST_POINT, start);
        let cancelled_generation = state.generation();
        state.cancel();

        assert_ne!(state.generation(), cancelled_generation);
        assert_eq!(
            state.on_timer(start + DEFAULT_DWELL_DELAY),
            DwellTimerEvent::Inactive
        );
    }

    #[test]
    fn preview_departure_uses_a_bounded_fast_reshow_window() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);

        assert_eq!(
            state.restart_after_preview(FIRST_POINT, start),
            PREVIEW_RESHOW_DELAY
        );
        assert_eq!(
            state.restart(SECOND_POINT, start + Duration::from_millis(200)),
            PREVIEW_RESHOW_DELAY
        );
        assert_eq!(
            state.on_timer(start + Duration::from_millis(250)),
            DwellTimerEvent::Candidate(DwellCandidate {
                generation: Generation::from_raw(2),
                anchor: SECOND_POINT,
                point: SECOND_POINT,
                pointer_span: PhysicalScreenSpan::from_point(SECOND_POINT),
            })
        );

        assert_eq!(
            state.restart(FIRST_POINT, start + PREVIEW_RESHOW_GRACE),
            DEFAULT_DWELL_DELAY,
            "ordinary motion must not extend the fixed re-show grace period"
        );
    }

    #[test]
    fn cancellation_and_success_clear_fast_reshow_state() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);

        state.restart_after_preview(FIRST_POINT, start);
        state.cancel();
        assert_eq!(
            state.restart(SECOND_POINT, start + Duration::from_millis(10)),
            DEFAULT_DWELL_DELAY
        );

        state.restart_after_preview(FIRST_POINT, start + Duration::from_millis(20));
        state.preview_shown();
        assert_eq!(
            state.restart(SECOND_POINT, start + Duration::from_millis(30)),
            DEFAULT_DWELL_DELAY
        );
    }

    #[test]
    fn generation_wraps_without_panicking() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);
        state.generation = Generation::from_raw(u64::MAX);

        state.restart(FIRST_POINT, start);

        assert_eq!(
            state.on_timer(start + DEFAULT_DWELL_DELAY),
            DwellTimerEvent::Candidate(DwellCandidate {
                generation: Generation::from_raw(0),
                anchor: FIRST_POINT,
                point: FIRST_POINT,
                pointer_span: PhysicalScreenSpan::from_point(FIRST_POINT),
            })
        );
    }

    #[test]
    fn tracked_span_must_stay_inside_the_resolved_item() {
        let start = Instant::now();
        let mut state = HoverState::new(DEFAULT_DWELL_DELAY);
        state.restart(PhysicalScreenPoint::new(110, 120), start);
        state.track_motion(PhysicalScreenPoint::new(190, 180));
        let generation = state.generation();

        assert!(state.tracked_span_fits(
            generation,
            PhysicalScreenRect::try_new(100, 100, 200, 200).unwrap()
        ));
        assert!(!state.tracked_span_fits(
            generation,
            PhysicalScreenRect::try_new(120, 100, 200, 200).unwrap()
        ));
        assert!(!state.tracked_span_fits(
            Generation::from_raw(generation.get() + 1),
            PhysicalScreenRect::try_new(100, 100, 200, 200).unwrap()
        ));

        state.finish_resolution(generation);
        assert_eq!(state.tracking_anchor(), None);
    }
}
