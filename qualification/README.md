# Milestone 1 qualification

CursorPeek does not connect hover resolution to the preview window until the resolver and window
spikes pass one combined gate. The gate consumes raw resolver result files and a separately
observed preview-window TSV:

```powershell
.\tools\Test-Milestone1Gate.ps1 `
    -ResolverResultsDirectory .\corpus\results `
    -WindowEvidence .\qualification\evidence\window.tsv `
    -Report .\qualification\milestone-1-report.md
```

Raw sessions belong in ignored `corpus/sessions`, `corpus/results`, and
`qualification/evidence` directories. Keep them with VM snapshots, display topology, Explorer
setup notes, and file hashes. The public aggregate report records only file names, hashes, counts,
coverage, latency, and failed or missing gates; it does not copy private local paths.

## Resolver evidence

Produce resolver results with `tools/Test-ResolverCorpus.ps1`. The combined gate parses the exact
17-column result schema again, recomputes every verdict, compares paths with Windows ordinal
case-insensitive semantics, and rejects duplicate labeled point observations even when their case
IDs differ. It does not require VM-local expected paths to exist on the machine running the
aggregator.

A pass requires:

- at least 2,000 independent labeled rows across Windows 10 and Windows 11;
- at least 99.9% correct supported mappings and zero wrong paths;
- no runner timeout or crash;
- resolver p95 no greater than 50 ms;
- every checked-in resolver scenario and 100/125/150/175/200% DPI on each supported OS.

Repeated probes at one unchanged point may diagnose warm latency, but they are not independent
qualification evidence.

`-ResolverResultsDirectory` reads top-level `*.results.tsv` files in ordinal name order and is the
recommended form for `powershell.exe -File`, which cannot bind multiple native command-line
arguments reliably to a PowerShell array parameter. `-ResolverResults` remains useful for one file
or an array supplied from an interactive PowerShell session.

## Preview-window evidence

Create the window TSV with this exact header:

```text
case_id	os	build	dpi	layout	scenario	interaction	focus_preserved	mouse_activation_eaten	click_delivered	dismissed	inside_work_area	pointer_gap_preserved	ui_thread_max_us	notes
```

Use `tools\Measure-PreviewWindow.ps1` to append one observation at a time. A click is delivered
only when the operator sees the intended Explorer action happen exactly once: selection for a left
click or the intended context menu for a right click. The private executable diagnostic supplies
final focus, dismissal, placement, pointer-gap, and UI-thread timing observations; it never
manufactures click-delivery evidence.

Each OS needs 100/125/150/175/200% DPI, timeout/move/wheel/left-click/right-click interactions, and
these scenario labels:

- `center`
- `work_area_top_left`
- `work_area_top_right`
- `work_area_bottom_left`
- `work_area_bottom_right`
- `negative_origin_monitor`
- `mixed_dpi_transition`
- `explorer_restart`

Every retained row must preserve focus, retain the `MA_NOACTIVATEANDEAT` policy, dismiss as
expected, remain in the work area, preserve the pointer gap, keep its longest measured UI-thread
task at or below 16 ms, and deliver click interactions exactly once.

The schema examples are deliberately labeled `example` and `schema_example_not_evidence`. They
only test parsers:

```powershell
.\tools\Test-Milestone1Gate.ps1 `
    -ResolverResults .\qualification\schema-example.resolver.tsv `
    -WindowEvidence .\qualification\schema-example.window.tsv `
    -ValidateOnly
```

Running the examples without `-ValidateOnly` must fail the real gate.
