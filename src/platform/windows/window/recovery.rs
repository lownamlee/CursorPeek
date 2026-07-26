use std::{
    collections::BTreeSet,
    fmt,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::WPARAM,
    UI::WindowsAndMessaging::{PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, WM_POWERBROADCAST},
};

use crate::{
    hover::Generation,
    worker::{WorkerManager, WorkerManagerError, run_timeout_diagnostic},
};

use super::{
    ApplicationRunError, MessageWindow, TASKBAR_CREATED_MESSAGE,
    system_lifecycle_change_for_message,
};

const RECOVERY_SOAK_CYCLES: u32 = 32;
const RECOVERY_SOAK_PERIOD: u32 = 8;

impl MessageWindow {
    pub(crate) fn run_recovery_soak_diagnostics(
        mut self,
    ) -> Result<RecoverySoakReport, ApplicationRunError> {
        self.worker_manager = Some(WorkerManager::start_recovery_diagnostics()?);
        let operation = self.execute_recovery_soak();
        let shutdown = self.shutdown_application();

        match (operation, shutdown) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error.into()),
            (Err(operation), Err(shutdown)) => Err(WorkerManagerError::ShutdownAfterFailure {
                operation: Box::new(operation),
                shutdown: Box::new(shutdown),
            }
            .into()),
        }
    }

    fn execute_recovery_soak(&mut self) -> Result<RecoverySoakReport, WorkerManagerError> {
        let started = Instant::now();
        let mut generation = 1_u64;
        let mut requests = 0_u32;
        let mut taskbar_recoveries = 0_u32;
        let mut power_cycles = 0_u32;
        let mut idle_restarts = 0_u32;
        let mut forced_timeouts = 0_u32;
        let mut sessions = BTreeSet::new();

        for cycle in 1..=RECOVERY_SOAK_CYCLES {
            let first = self.resolve_diagnostic_session(generation)?;
            generation += 1;
            requests += 1;
            sessions.insert(first);

            let reused = self.resolve_diagnostic_session(generation)?;
            generation += 1;
            requests += 1;
            sessions.insert(reused);
            if reused != first {
                return Err(WorkerManagerError::SessionNotReused);
            }

            let taskbar_created = TASKBAR_CREATED_MESSAGE.load(Ordering::Acquire);
            self.apply_recovery_message(taskbar_created, WPARAM(0))?;
            taskbar_recoveries += 1;

            let after_explorer_restart = self.resolve_diagnostic_session(generation)?;
            generation += 1;
            requests += 1;
            sessions.insert(after_explorer_restart);
            if after_explorer_restart == reused {
                return Err(WorkerManagerError::SessionNotRecycled);
            }

            self.apply_recovery_message(WM_POWERBROADCAST, WPARAM(PBT_APMSUSPEND as usize))?;
            if !self.power_suspended {
                return Err(WorkerManagerError::UnexpectedLifecycleState);
            }
            self.apply_recovery_message(
                WM_POWERBROADCAST,
                WPARAM(PBT_APMRESUMEAUTOMATIC as usize),
            )?;
            if self.power_suspended {
                return Err(WorkerManagerError::UnexpectedLifecycleState);
            }
            power_cycles += 1;

            let after_power_cycle = self.resolve_diagnostic_session(generation)?;
            generation += 1;
            requests += 1;
            sessions.insert(after_power_cycle);
            if after_power_cycle == after_explorer_restart {
                return Err(WorkerManagerError::SessionNotRecycled);
            }

            if cycle % RECOVERY_SOAK_PERIOD == 0 {
                let expired = self
                    .worker_manager
                    .as_ref()
                    .expect("the recovery soak retains its worker manager")
                    .wait_for_diagnostic_idle_expiry()?;
                if expired != after_power_cycle {
                    return Err(WorkerManagerError::UnexpectedIdleSession);
                }

                let after_idle = self.resolve_diagnostic_session(generation)?;
                generation += 1;
                requests += 1;
                sessions.insert(after_idle);
                if after_idle == after_power_cycle {
                    return Err(WorkerManagerError::SessionNotRestarted);
                }
                idle_restarts += 1;

                run_timeout_diagnostic()?;
                forced_timeouts += 1;
            }
        }

        Ok(RecoverySoakReport {
            elapsed: started.elapsed(),
            cycles: RECOVERY_SOAK_CYCLES,
            requests,
            sessions: u32::try_from(sessions.len())
                .expect("the fixed recovery soak session count fits u32"),
            taskbar_recoveries,
            power_cycles,
            idle_restarts,
            forced_timeouts,
        })
    }

    fn resolve_diagnostic_session(&self, generation: u64) -> Result<u64, WorkerManagerError> {
        self.worker_manager
            .as_ref()
            .expect("the recovery soak retains its worker manager")
            .resolve_diagnostic_session(Generation::from_raw(generation))
    }

    fn apply_recovery_message(
        &mut self,
        message: u32,
        parameter: WPARAM,
    ) -> Result<(), WorkerManagerError> {
        let change = system_lifecycle_change_for_message(message, parameter)
            .ok_or(WorkerManagerError::UnexpectedLifecycleSignal)?;
        self.handle_system_lifecycle_change(change)
            .map_err(WorkerManagerError::Lifecycle)
    }
}

pub(crate) struct RecoverySoakReport {
    elapsed: Duration,
    cycles: u32,
    requests: u32,
    sessions: u32,
    taskbar_recoveries: u32,
    power_cycles: u32,
    idle_restarts: u32,
    forced_timeouts: u32,
}

impl fmt::Display for RecoverySoakReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Recovery soak completed: cycles={}, requests={}, sessions={}, \
             taskbar_recoveries={}, power_cycles={}, idle_restarts={}, forced_timeouts={}, \
             residual_workers=0, elapsed={} ms",
            self.cycles,
            self.requests,
            self.sessions,
            self.taskbar_recoveries,
            self.power_cycles,
            self.idle_restarts,
            self.forced_timeouts,
            self.elapsed.as_millis()
        )
    }
}
