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

## Evidence scope

The checked-in matrix is a coverage checklist, not test results. A release decision must cite raw
session files produced in the named environments. Generated, copied, or relabeled rows are not
valid corpus evidence.
