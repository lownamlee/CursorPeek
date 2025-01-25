mod coverage;
mod state;

pub(crate) use coverage::{
    InputCoverage, InputCoverageReport, INPUT_DIAGNOSTIC_DURATION, INPUT_SAMPLE_INTERVAL,
};
pub(crate) use state::{
    DwellTimerEvent, HoverRectangle, HoverState, PhysicalScreenPoint, DEFAULT_DWELL_DELAY,
};
