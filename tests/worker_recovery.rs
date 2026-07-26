use std::{
    process::Command,
    sync::{Mutex, MutexGuard},
};

static DIAGNOSTIC_LOCK: Mutex<()> = Mutex::new(());

fn diagnostic_guard() -> MutexGuard<'static, ()> {
    DIAGNOSTIC_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn contained_worker_reuses_restarts_and_recycles_on_demand() {
    let _guard = diagnostic_guard();
    let output = Command::new(env!("CARGO_BIN_EXE_CursorPeek"))
        .arg("--worker-diagnostics")
        .output()
        .expect("the worker diagnostic should start");

    assert!(
        output.status.success(),
        "diagnostic failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(
        "Contained worker diagnostic completed: final_generation=4, status=Unavailable, \
         requests=4, sessions=3, reuse=yes, idle_restart=yes, session_recycle=yes"
    ));
}

#[test]
fn contained_worker_timeout_is_terminated_and_reaped() {
    let _guard = diagnostic_guard();
    let output = Command::new(env!("CARGO_BIN_EXE_CursorPeek"))
        .arg("--worker-timeout-diagnostics")
        .output()
        .expect("the timeout diagnostic should start");

    assert!(
        output.status.success(),
        "timeout diagnostic failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Contained worker timeout cleanup completed."
    );
}

#[test]
fn shell_and_power_recovery_soak_reaps_every_worker() {
    let _guard = diagnostic_guard();
    let output = Command::new(env!("CARGO_BIN_EXE_CursorPeek"))
        .arg("--recovery-soak-diagnostics")
        .output()
        .expect("the recovery soak diagnostic should start");

    assert!(
        output.status.success(),
        "recovery soak failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with(
            "Recovery soak completed: cycles=32, requests=132, sessions=69, \
             taskbar_recoveries=32, power_cycles=32, idle_restarts=4, forced_timeouts=4, \
             residual_workers=0, elapsed="
        ),
        "unexpected recovery soak report: {stdout}"
    );
    assert!(
        stdout.trim_end().ends_with(" ms"),
        "recovery soak report must end with a bounded elapsed measurement: {stdout}"
    );
}
