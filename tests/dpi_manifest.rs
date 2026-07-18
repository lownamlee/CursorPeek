use std::process::Command;

#[test]
fn embedded_manifest_enables_per_monitor_v2() {
    let output = Command::new(env!("CARGO_BIN_EXE_CursorPeek"))
        .arg("--dpi-diagnostics")
        .output()
        .expect("the DPI diagnostic should start");

    assert!(
        output.status.success(),
        "DPI diagnostic failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Per-Monitor V2 DPI awareness is active."
    );
}
