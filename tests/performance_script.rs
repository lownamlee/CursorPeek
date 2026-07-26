use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn create() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the test clock should follow the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cursorpeek-performance-script-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("the performance script fixture should be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn performance_gate_rejects_an_oversized_executable_before_launch() {
    let fixture = TestDirectory::create();
    let executable = fixture.path().join("CursorPeek.exe");
    File::create(&executable)
        .expect("the oversized executable fixture should be created")
        .set_len((2 * 1024 * 1024) + 1)
        .expect("the oversized executable fixture should be extended");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tools")
        .join("Test-PerformanceBudget.ps1");

    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .arg("-Executable")
        .arg(&executable)
        .output()
        .expect("Windows PowerShell should run the performance gate");

    assert!(!output.status.success());
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        diagnostic.contains("Release executable size"),
        "the gate should identify the failed artifact budget: {diagnostic}"
    );
}
