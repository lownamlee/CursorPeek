use std::process::Command;

#[test]
fn noactivate_preview_diagnostic_preserves_focus_and_eats_clicks() {
    let output = Command::new(env!("CARGO_BIN_EXE_CursorPeek"))
        .arg("--preview-window-diagnostics")
        .output()
        .expect("the preview-window diagnostic should start");

    assert!(
        output.status.success(),
        "preview-window diagnostic failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No-activate preview diagnostic completed:"));
    assert!(stdout.contains("focus_preserved=yes"));
    assert!(stdout.contains("mouse_activation=eaten"));
    assert!(stdout.contains("dismissal="));
    assert!(stdout.contains("inside_work_area=yes"));
    assert!(stdout.contains("pointer_gap_preserved=yes"));
    assert!(stdout.contains("ui_thread_max_us="));
}
