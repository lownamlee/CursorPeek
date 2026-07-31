# CursorPeek 1,500-Run Stress Report

Date: 2026-07-31
Target: CursorPeek v0.4.0 at `E:\CursorPeek-stress-20260728-134821\installed\CursorPeek.exe`
Scope: local executable, local fixtures, WSL fuzz targets, and local diagnostic modes. No GitHub actions were performed.

## Result

The requested run was stopped after the user said the coverage was sufficient. No confirmed data-loss, code-execution, or privilege-impacting failure was observed in the completed coverage.

## Completed 1,500-Run Cases

- libFuzzer: `protocol`, `payload`, `content_sniff`, `layout`, and `svg` (1,500 each; 7,500 total); all exited successfully.
- Installed executable: `--help`, `--version`, `--dpi-diagnostics`, `--settings-diagnostics`, `--worker-diagnostics`, `--worker-timeout-diagnostics`, and `--preview-window-diagnostics` (1,500 each); all passed their output and stderr assertions.
- Preview-window practice diagnostic: 1,500 runs at normal four-process concurrency; all passed.

## Partial / Additional Coverage

- Recovery soak: 1,200 of 1,500 completed before the run was stopped; no failure line or residual-worker error was recorded.
- High-concurrency preview practice probe: first failure at iteration 352 with 16 simultaneous launches. The process returned exit code 0 but wrote `CursorPeek failed: The operation completed successfully. (0x00000000)` to stderr. The same functionality passed 1,500 runs at four-process concurrency, so this is recorded as a concurrency-pressure signal rather than a confirmed functional failure.

## Not Run After User Stop

- Remaining 300 recovery-soak iterations.
- Performance, startup registration toggles, shutdown-existing, ordinary tray lifecycle, raw-input diagnostics, Explorer hover across every media family, and interactive tray-menu actions.

## Evidence

The raw per-run evidence remains in `E:\CursorPeek-stress-20260728-134821\stress-1500-20260731`. A compact review archive containing all summaries, fuzz logs, and the high-concurrency failure capture is `E:\CursorPeek-stress-20260728-134821\CursorPeek-stress-1500-summary-20260731.zip`. The process cleanup check after stopping reported zero remaining `CursorPeek.exe` processes.

This report and archive are local artifacts only; they were not committed or pushed to GitHub.
