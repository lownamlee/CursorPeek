# Explorer resolver corpus

This directory defines the evidence needed to qualify CursorPeek's point-to-file resolver. The
normal application protocol deliberately returns only a status. A disabled-by-default
`resolver-corpus` feature adds a private disposable probe that reports the exact resolved path and
a stable failure reason for measurement.

## Session format

Create one UTF-8 TSV file per environment with this exact header:

```text
case_id	os	build	dpi	layout	scenario	x	y	expectation	expected_path
```

- `case_id` is a unique unsigned integer.
- `os` is `windows10` or `windows11`; `build` identifies the exact OS build.
- `dpi`, `layout`, and `scenario` are nonempty labels from the scenario matrix.
- `x` and `y` are physical desktop coordinates and may be negative.
- `expectation` is `resolve` for a supported file target or `fail_closed` for a negative target.
- `expected_path` is an existing drive-absolute file for `resolve` and empty for `fail_closed`.

Coordinates describe one prepared Explorer session; they are not portable between machines or
window arrangements. Keep each raw session with its VM/display notes. Do not duplicate rows to
reach the sample threshold.

`schema-example.tsv` exists only to exercise manifest validation and runner plumbing. Its fabricated
environment label and coordinate are not resolver evidence.

## Run a session

From the repository root:

```powershell
.\tools\Test-ResolverCorpus.ps1 -Manifest .\corpus\sessions\win10-details.tsv
```

The runner builds a feature-gated release probe, waits five seconds so Explorer can become the
foreground window, then sends each labeled point to one reusable probe. Every native call has a
deadline; a timeout kills the disposable process, records the failure, and starts a clean probe for
the next case. Raw results default to `target\resolver-corpus\results`.

Use `-ValidateOnly` to check a manifest without starting a probe. Use `-Evaluate` only for a
combined corpus intended to satisfy the release gate; it requires at least 2,000 rows across both
Windows 10 and Windows 11, at least 99.9% correct supported mappings, zero wrong paths, no probe
failures, and resolver p95 no greater than 50 ms.

The final Milestone 1 decision is stricter than one runner invocation. Pass every raw result file
to `tools\Test-Milestone1Gate.ps1` with the separately collected preview-window evidence. That
aggregator recomputes verdicts, rejects repeated labeled points, checks every scenario and DPI on
both supported operating systems, and emits the hash-addressed Markdown report used for the
branch decision. See `qualification\README.md`.

The runner compares Windows paths with ordinal case-insensitive semantics. A positive miss reduces
coverage. A mismatched positive path or any resolved `fail_closed` case is a wrong-path failure.

## Capture visible file pages

Create a disposable deterministic folder when a larger real-item inventory is needed:

```powershell
.\tools\New-ResolverFixture.ps1 `
    -Destination C:\Users\Public\CursorPeekCorpus\bulk-001 `
    -FileCount 256
```

The destination must be absent or empty. The script never removes or overwrites a fixture and
writes a SHA-256 inventory beside it.

For a prepared foreground Explorer page, the live collector can label visible local files and run
the same feature-gated probe:

```powershell
.\tools\Measure-ExplorerResolverPage.ps1 `
    -FixturePath C:\Users\Public\CursorPeekCorpus\bulk-001 `
    -Os windows11 `
    -Build 22631 `
    -Dpi 175 `
    -Layout details `
    -Scenario file_row `
    -CaseIdStart 1200000 `
    -SessionName win11-22631-175-details-page-01 `
    -PointProfile row_three
```

The collector searches only the exact Explorer frame's bounded UI Automation subtree and excludes
items clipped by their list viewport. For every fully visible `ListItem` or `DataItem`, it obtains a
physical clickable point, sends one ordinary click, requires that exact frame to become foreground,
and accepts an expected path only when Explorer's separate `SelectedItems()` model reports one
existing drive-local non-folder file after a bounded selection-settling interval. It fingerprints
the page before and after labeling so a selection-induced scroll, reorder, or geometry change
aborts the session. It then writes a manifest, raw probe results, numeric OS/DPI/view state, hashes,
and a frame screenshot under
`target\resolver-corpus\live`. A miss or wrong path remains in the raw result and fails the
collector.

For a high-count Details-page session, `-PointProfile item_grid` samples a bounded physical grid
inside every fully visible item. The default 32-pixel edge inset avoids the Windows 10 item-checkbox
region; spacing, row count, per-item points, and total cases all have strict caps and are recorded in
`state.json`. Every grid coordinate is clicked separately and must produce the same one-item Shell
selection before it receives that file's label:

```powershell
.\tools\Measure-ExplorerResolverPage.ps1 `
    -FixturePath C:\Users\Public\CursorPeekCorpus\bulk-001 `
    -Os windows10 `
    -Build 19045 `
    -Dpi 100 `
    -Layout details `
    -Scenario file_row `
    -CaseIdStart 1306000 `
    -SessionName win10-19045-100-details-grid-01 `
    -PointProfile item_grid `
    -GridSpacingPixels 16 `
    -GridRows 1 `
    -MaxCases 4096
```

Grid sessions take longer because they do not multiply one observed label into unverified points.
They improve point-count and row-region coverage but do not replace other layouts, negative
targets, DPI values, tabs, restart, or display-topology scenarios.

This tool captures positive visible-file pages only. It does not prove special scenarios such as
inactive tabs, namespace targets, touch, mixed DPI, or negative-origin monitors. It also never
copies output into `corpus\results` or another accepted-evidence path. Review each page and its
screenshot first. After scrolling or changing layout/DPI, use a fresh session name and disjoint
case-ID range, and relabel the new live page rather than copying or editing prior rows.

## Evidence scope

The checked-in matrix is a coverage checklist, not test results. A release decision must cite raw
session files produced in the named environments. Generated, copied, or relabeled rows are not
valid corpus evidence.
