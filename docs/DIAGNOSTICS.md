# Diagnostic build

The diagnostic build is an optimized CursorPeek executable with structured timing and failure
tracing enabled. Use it when a manual Explorer interaction is slow, fails, or behaves differently
from an automated check.

Build it with:

```powershell
cargo build --locked --release --features diagnostic-log
```

Logs are written to:

```text
%LOCALAPPDATA%\CursorPeek\diagnostics\<run-id>\
```

`latest-run.txt` identifies the newest run. The coordinator and each contained preview worker write
separate JSONL files in the same run directory. Their `qpc` timestamps use the same Windows
high-resolution performance counter, so events can be correlated across process boundaries.

The trace covers pointer input, dwell scheduling, Explorer identity, resolver stages, file
validation, provider and cache decisions, decoding, IPC, preview layout/show timing, lifecycle
events, and bounded error categories. It does not record file contents, file names, complete paths,
authentication data, or environment variables.

The writer runs on a dedicated thread, uses a bounded queue, flushes important milestones promptly,
and caps each process log at 64 MiB. A `logger.summary` record reports dropped events. Use
`tools/Summarize-DiagnosticLog.ps1` to calculate correlated display latency:

```powershell
.\tools\Summarize-DiagnosticLog.ps1
.\tools\Summarize-DiagnosticLog.ps1 -Timeline
```

Exit CursorPeek normally after reproducing an issue when possible. This writes the final summary,
although preview milestones are flushed while the process is still running.

Logging is observational: failure to create the log does not prevent CursorPeek from running. If
`latest-run.txt` is absent or unchanged after starting the diagnostic executable, report that as
the diagnostic failure.
