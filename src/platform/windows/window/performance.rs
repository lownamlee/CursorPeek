use std::{
    fmt,
    time::{Duration, Instant},
};

use crate::{settings::SettingsDocument, worker::WorkerManager};

use super::{
    ApplicationRunError, MessageLoopExit, MessageWindow, PERFORMANCE_DIAGNOSTIC_DEADLINE_TIMER_ID,
    TRAY_CALLBACK_MESSAGE, TrayIcon, WindowTimer,
};

const PERFORMANCE_DIAGNOSTIC_HOLD: Duration = Duration::from_millis(750);

pub(super) struct ActivePerformanceDiagnostics {
    pub(super) ready_elapsed: Duration,
    pub(super) ui_thread_max: Duration,
    pub(super) _deadline_timer: WindowTimer,
}

impl MessageWindow {
    pub(crate) fn run_performance_diagnostics(
        mut self,
        worker_manager: WorkerManager,
        settings_document: SettingsDocument,
        startup_started: Instant,
    ) -> Result<PerformanceDiagnosticReport, ApplicationRunError> {
        self.worker_manager = Some(worker_manager);
        // Defaults exercise the tray/settings presentation without creating configuration or
        // reconciling the current user's startup registration.
        self.settings_document = Some(settings_document);
        self.tray_icon = Some(TrayIcon::create(self.hwnd, TRAY_CALLBACK_MESSAGE)?);

        let mut deadline_timer =
            WindowTimer::new(self.hwnd, PERFORMANCE_DIAGNOSTIC_DEADLINE_TIMER_ID);
        deadline_timer.arm(PERFORMANCE_DIAGNOSTIC_HOLD)?;
        self.performance_diagnostics = Some(ActivePerformanceDiagnostics {
            // Reaching this boundary means every measured idle coordinator component is live.
            ready_elapsed: startup_started.elapsed(),
            ui_thread_max: Duration::ZERO,
            _deadline_timer: deadline_timer,
        });

        let loop_result = self.run_loop();
        let shutdown_result = self.shutdown_application();
        let exit = loop_result?;
        shutdown_result?;

        match exit {
            MessageLoopExit::PerformanceDiagnostics(report) => Ok(report),
            MessageLoopExit::Shutdown => Ok(self.finish_performance_diagnostics()),
            MessageLoopExit::InputDiagnostics(_) | MessageLoopExit::PreviewDiagnostics(_) => {
                unreachable!("the performance diagnostic cannot run another diagnostic")
            }
        }
    }

    pub(super) fn finish_performance_diagnostics(&mut self) -> PerformanceDiagnosticReport {
        let diagnostics = self
            .performance_diagnostics
            .take()
            .expect("the performance diagnostic exits only while active");
        PerformanceDiagnosticReport {
            ready_elapsed: diagnostics.ready_elapsed,
            ui_thread_max: diagnostics.ui_thread_max,
        }
    }
}

pub(crate) struct PerformanceDiagnosticReport {
    ready_elapsed: Duration,
    ui_thread_max: Duration,
}

impl fmt::Display for PerformanceDiagnosticReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ready_us = u64::try_from(self.ready_elapsed.as_micros()).unwrap_or(u64::MAX);
        let ui_thread_max_us = u64::try_from(self.ui_thread_max.as_micros()).unwrap_or(u64::MAX);
        write!(
            formatter,
            "Idle startup diagnostic completed: ready_us={ready_us}, hold_ms={}, \
             idle_ui_thread_max_us={ui_thread_max_us}, graceful_shutdown=yes",
            PERFORMANCE_DIAGNOSTIC_HOLD.as_millis()
        )
    }
}
