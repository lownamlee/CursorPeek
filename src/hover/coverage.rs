use std::{fmt, time::Duration};

use super::PhysicalScreenPoint;

pub(crate) const INPUT_DIAGNOSTIC_DURATION: Duration = Duration::from_secs(30);
pub(crate) const INPUT_SAMPLE_INTERVAL: Duration = Duration::from_millis(125);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct InputCoverageReport {
    raw_movement_packets: u64,
    raw_interruption_packets: u64,
    active_samples: u64,
    changed_samples: u64,
    unmatched_changes: u64,
}

impl InputCoverageReport {
    #[cfg(test)]
    pub(crate) fn active_samples(self) -> u64 {
        self.active_samples
    }

    #[cfg(test)]
    pub(crate) fn changed_samples(self) -> u64 {
        self.changed_samples
    }

    #[cfg(test)]
    pub(crate) fn unmatched_changes(self) -> u64 {
        self.unmatched_changes
    }
}

impl fmt::Display for InputCoverageReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "Raw Input movement packets: {}",
            self.raw_movement_packets
        )?;
        writeln!(
            formatter,
            "Raw Input button/wheel packets: {}",
            self.raw_interruption_packets
        )?;
        writeln!(
            formatter,
            "Foreground-Explorer physical samples: {}",
            self.active_samples
        )?;
        writeln!(
            formatter,
            "Physical samples that changed position: {}",
            self.changed_samples
        )?;
        write!(
            formatter,
            "Changed samples without intervening Raw Input movement: {}",
            self.unmatched_changes
        )
    }
}

#[derive(Debug, Default)]
pub(crate) struct InputCoverage {
    report: InputCoverageReport,
    previous_point: Option<PhysicalScreenPoint>,
    raw_movement_since_sample: bool,
}

impl InputCoverage {
    pub(crate) fn observe_raw(
        &mut self,
        point: PhysicalScreenPoint,
        moved: bool,
        interrupted: bool,
    ) {
        if moved {
            self.report.raw_movement_packets = self.report.raw_movement_packets.saturating_add(1);
        }
        if interrupted {
            self.report.raw_interruption_packets =
                self.report.raw_interruption_packets.saturating_add(1);
        }

        if self.previous_point.is_none() {
            self.previous_point = Some(point);
            self.raw_movement_since_sample = false;
        } else if moved {
            self.raw_movement_since_sample = true;
        }
    }

    pub(crate) fn observe_sample(&mut self, point: PhysicalScreenPoint) {
        let Some(previous_point) = self.previous_point.replace(point) else {
            self.raw_movement_since_sample = false;
            return;
        };

        self.report.active_samples = self.report.active_samples.saturating_add(1);
        if point != previous_point {
            self.report.changed_samples = self.report.changed_samples.saturating_add(1);
            if !self.raw_movement_since_sample {
                self.report.unmatched_changes = self.report.unmatched_changes.saturating_add(1);
            }
        }

        self.raw_movement_since_sample = false;
    }

    pub(crate) fn suspend(&mut self) {
        self.previous_point = None;
        self.raw_movement_since_sample = false;
    }

    #[cfg(test)]
    pub(crate) fn is_active(&self) -> bool {
        self.previous_point.is_some()
    }

    pub(crate) fn report(&self) -> InputCoverageReport {
        self.report
    }
}

#[cfg(test)]
mod tests {
    use super::{InputCoverage, InputCoverageReport};
    use crate::hover::PhysicalScreenPoint;

    const ORIGIN: PhysicalScreenPoint = PhysicalScreenPoint { x: 0, y: 0 };

    #[test]
    fn first_raw_packet_establishes_a_baseline() {
        let mut coverage = InputCoverage::default();

        coverage.observe_raw(ORIGIN, true, false);
        coverage.observe_sample(ORIGIN);

        assert_eq!(
            coverage.report(),
            InputCoverageReport {
                raw_movement_packets: 1,
                active_samples: 1,
                ..InputCoverageReport::default()
            }
        );
    }

    #[test]
    fn raw_movement_matches_the_next_changed_sample() {
        let mut coverage = InputCoverage::default();
        coverage.observe_raw(ORIGIN, true, false);

        coverage.observe_raw(PhysicalScreenPoint::new(4, -2), true, false);
        coverage.observe_sample(PhysicalScreenPoint::new(4, -2));

        assert_eq!(
            coverage.report(),
            InputCoverageReport {
                raw_movement_packets: 2,
                active_samples: 1,
                changed_samples: 1,
                ..InputCoverageReport::default()
            }
        );
    }

    #[test]
    fn physical_change_without_raw_movement_is_reported_as_unmatched() {
        let mut coverage = InputCoverage::default();
        coverage.observe_raw(ORIGIN, false, true);

        coverage.observe_sample(PhysicalScreenPoint::new(10, 0));

        assert_eq!(
            coverage.report(),
            InputCoverageReport {
                raw_interruption_packets: 1,
                active_samples: 1,
                changed_samples: 1,
                unmatched_changes: 1,
                ..InputCoverageReport::default()
            }
        );
    }

    #[test]
    fn raw_motion_that_returns_to_the_same_sample_point_is_not_a_change() {
        let mut coverage = InputCoverage::default();
        coverage.observe_raw(ORIGIN, true, false);

        coverage.observe_raw(PhysicalScreenPoint::new(8, 0), true, false);
        coverage.observe_sample(ORIGIN);

        assert_eq!(
            coverage.report(),
            InputCoverageReport {
                raw_movement_packets: 2,
                active_samples: 1,
                ..InputCoverageReport::default()
            }
        );
    }

    #[test]
    fn suspension_discards_cross_window_position_deltas() {
        let mut coverage = InputCoverage::default();
        coverage.observe_raw(ORIGIN, true, false);
        coverage.suspend();
        assert!(!coverage.is_active());

        let resumed = PhysicalScreenPoint::new(900, 600);
        coverage.observe_raw(resumed, true, false);
        coverage.observe_sample(resumed);

        assert_eq!(
            coverage.report(),
            InputCoverageReport {
                raw_movement_packets: 2,
                active_samples: 1,
                ..InputCoverageReport::default()
            }
        );
    }
}
