# Windows qualification

CursorPeek qualifies its Explorer resolver and preview behavior from retained observations on
Windows 10 22H2 and Windows 11. The aggregate gate reparses the raw TSV files, recomputes resolver
verdicts, rejects duplicate point observations, and writes a path-free report with evidence
hashes:

```powershell
.\tools\Test-WindowsQualification.ps1 `
    -ResolverResultsDirectory .\corpus\results `
    -WindowEvidence .\qualification\evidence\window.tsv `
    -Report .\qualification\windows-release-qualification.md
```

Raw sessions belong in the ignored `corpus/sessions`, `corpus/results`, and
`qualification/evidence` directories. Retain them with VM snapshots, display topology, Explorer
setup notes, and hashes. The checked-in
[`windows-release-qualification.md`](windows-release-qualification.md) records the aggregate gate;
[`windows-runtime-observations.md`](windows-runtime-observations.md) records the supplemental
topology, theme, lifecycle, and input observations.

## Coverage model

The release matrix is risk-based, not a Cartesian product:

- Windows 10 and Windows 11 each require the core file-icon, file-row, fail-closed surface,
  folder, multiple-window, and Explorer-restart resolver cases.
- Windows 11 additionally requires active/background tab ambiguity cases.
- The combined resolver corpus requires 100%, 125%, 150%, 175%, and 200% DPI plus genuine
  negative-coordinate and mixed-DPI observations.
- Each OS requires center, two opposite work-area corners, Explorer restart, and
  timeout/move/wheel/left-click/right-click preview observations.
- The combined preview evidence requires the other two work-area corners, all five DPI values,
  a negative-coordinate monitor, and a mixed-DPI transition.

This pairwise boundary coverage exercises every release-critical dimension while avoiding
hundreds of redundant combinations. A row counts only when its environment and labeled point
were independently observed.

## Resolver evidence

`tools\Measure-ExplorerResolverPage.ps1` records a prepared Explorer page. The aggregate gate
requires:

- at least 2,000 independent labeled rows;
- at least 99.9% correct supported mappings;
- zero wrong paths and zero runner failures;
- p95 latency no greater than 50 ms;
- the OS, core-scenario, DPI, and topology coverage above.

The gate compares paths with Windows ordinal case-insensitive semantics. It does not require a
VM-local expected path to exist on the machine aggregating the evidence.

## Preview-window evidence

`tools\Measure-PreviewWindow.ps1` appends this schema:

```text
case_id	os	build	dpi	layout	scenario	interaction	focus_preserved	mouse_activation_eaten	click_delivered	dismissed	inside_work_area	pointer_gap_preserved	ui_thread_max_us	notes
```

Every retained observation must preserve Explorer focus, retain the
`MA_NOACTIVATEANDEAT` policy, dismiss as expected, stay within the monitor work area, preserve the
pointer gap, and deliver click interactions exactly once. The initial preview create/show task
must complete within 50 ms. The separate idle performance gate keeps the stricter 16 ms
steady-state UI-thread ceiling.

Use `-Practice` for a five-second rehearsal. Practice data is forced into an `attempt` file and
must not be supplied to the release gate.

## Schema checks

The examples are parser fixtures, not evidence:

```powershell
.\tools\Test-WindowsQualification.ps1 `
    -ResolverResults .\qualification\schema-example.resolver.tsv `
    -WindowEvidence .\qualification\schema-example.window.tsv `
    -ValidateOnly
```

Without `-ValidateOnly`, the examples must fail the release gate.
