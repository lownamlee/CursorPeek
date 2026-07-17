use std::process::Command;

#[test]
fn contained_worker_reuses_then_restarts_after_idle_expiry() {
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
        "Contained worker diagnostic completed: generation=1, status=Unavailable, requests=3, \
         sessions=2, reuse=yes, idle_restart=yes"
    ));
}

#[test]
fn contained_worker_timeout_is_terminated_and_reaped() {
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
