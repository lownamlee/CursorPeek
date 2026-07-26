mod coverage;
mod state;

pub(crate) use coverage::{
    INPUT_DIAGNOSTIC_DURATION, INPUT_SAMPLE_INTERVAL, InputCoverage, InputCoverageReport,
};
pub(crate) use cursorpeek_core::{Generation, PhysicalScreenPoint};
pub(crate) use state::{DEFAULT_DWELL_DELAY, DwellTimerEvent, HoverRectangle, HoverState};
