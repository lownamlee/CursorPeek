use std::process::Command;

#[test]
fn idle_startup_diagnostic_reports_bounded_ready_and_ui_work() {
    let output = Command::new(env!("CARGO_BIN_EXE_CursorPeek"))
        .arg("--performance-diagnostics")
        .output()
        .expect("the idle startup diagnostic should start");

    assert!(
        output.status.success(),
        "idle startup diagnostic failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = stdout
        .trim()
        .strip_prefix("Idle startup diagnostic completed: ready_us=")
        .expect("the diagnostic should report its ready time");
    let (ready_us, report) = report
        .split_once(", hold_ms=750, idle_ui_thread_max_us=")
        .expect("the diagnostic should report its fixed observation hold");
    let (ui_thread_max_us, graceful_shutdown) = report
        .split_once(", graceful_shutdown=")
        .expect("the diagnostic should report UI work and shutdown");

    assert!(
        ready_us.parse::<u64>().is_ok_and(|value| value > 0),
        "ready time should be a positive integer: {stdout}"
    );
    assert!(
        ui_thread_max_us.parse::<u64>().is_ok(),
        "UI task time should be an integer: {stdout}"
    );
    assert_eq!(graceful_shutdown, "yes");
}
