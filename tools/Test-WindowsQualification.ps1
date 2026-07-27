[CmdletBinding()]
param(
    [ValidateNotNullOrEmpty()]
    [string[]]$ResolverResults,

    [string]$ResolverResultsDirectory,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$WindowEvidence,

    [string]$ScenarioMatrix,

    [string]$Report,

    [switch]$ValidateOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-Sha256Hex {
    param([Parameter(Mandatory = $true)][string]$LiteralPath)

    $stream = [System.IO.File]::OpenRead($LiteralPath)
    try {
        $algorithm = [System.Security.Cryptography.SHA256]::Create()
        try {
            return [System.BitConverter]::ToString(
                $algorithm.ComputeHash($stream)
            ).Replace("-", "")
        }
        finally {
            $algorithm.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

if ([string]::IsNullOrWhiteSpace($ScenarioMatrix)) {
    $ScenarioMatrix = Join-Path $PSScriptRoot "..\corpus\scenarios.tsv"
}
$hasResolverResults = $null -ne $ResolverResults -and $ResolverResults.Count -ne 0
$hasResolverDirectory = -not [string]::IsNullOrWhiteSpace($ResolverResultsDirectory)
if ($hasResolverResults -eq $hasResolverDirectory) {
    throw "Provide exactly one of ResolverResults or ResolverResultsDirectory."
}

$tab = "`t"
$invariant = [System.Globalization.CultureInfo]::InvariantCulture
$integerStyle = [System.Globalization.NumberStyles]::AllowLeadingSign
$unsignedStyle = [System.Globalization.NumberStyles]::None
$ordinal = [System.StringComparer]::Ordinal
$ordinalIgnoreCase = [System.StringComparer]::OrdinalIgnoreCase

$resolverHeader = @(
    "case_id",
    "os",
    "build",
    "dpi",
    "layout",
    "scenario",
    "x",
    "y",
    "expectation",
    "expected_path",
    "actual_status",
    "actual_path",
    "elapsed_us",
    "reason",
    "context_a",
    "context_b",
    "verdict"
) -join $tab

$windowHeader = @(
    "case_id",
    "os",
    "build",
    "dpi",
    "layout",
    "scenario",
    "interaction",
    "focus_preserved",
    "mouse_activation_eaten",
    "click_delivered",
    "dismissed",
    "inside_work_area",
    "pointer_gap_preserved",
    "ui_thread_max_us",
    "notes"
) -join $tab

$scenarioHeader = @("scenario", "expectation", "layout", "notes") -join $tab
$requiredOperatingSystems = @("windows10", "windows11")
$requiredDpiValues = @("100", "125", "150", "175", "200")
$requiredPerOsWindowScenarios = @(
    "center",
    "work_area_top_left",
    "work_area_bottom_right",
    "explorer_restart"
)
$requiredAggregateWindowScenarios = @(
    "work_area_top_right",
    "work_area_bottom_left",
    "negative_origin_monitor",
    "mixed_dpi_transition"
)
$requiredWindowInteractions = @("timeout", "move", "wheel", "left_click", "right_click")
$requiredCoreResolverKeys = @(
    "file_icon|resolve|large_icons",
    "file_row|resolve|details",
    "blank_items_view|fail_closed|all",
    "navigation_tree|fail_closed|all",
    "address_bar|fail_closed|all",
    "command_bar|fail_closed|all",
    "column_header|fail_closed|details",
    "folder_item|fail_closed|all",
    "multiple_windows|resolve|multiple_windows",
    "explorer_restart|resolve|details"
)
$requiredWindows11ResolverKeys = @(
    "rapid_tab_switch|resolve|active_tab",
    "background_tab|fail_closed|background_tab"
)
$requiredTopologyResolverKeys = @(
    "negative_origin_file|resolve|details_negative_origin",
    "mixed_dpi_file|resolve|details_mixed_dpi"
)
$resolverLatencyBudgetUs = 50000
$previewInitialTaskBudgetUs = 50000

function Resolve-OneFile {
    param(
        [string]$Path,
        [string]$Description
    )

    $resolved = @(Resolve-Path -LiteralPath $Path -ErrorAction Stop)
    if ($resolved.Count -ne 1 -or
        -not [System.IO.File]::Exists($resolved[0].Path)) {
        throw "$Description must resolve to exactly one existing file: $Path"
    }
    return $resolved[0].Path
}

function Assert-SafeLabel {
    param(
        [string]$Value,
        [string]$Description
    )

    if ($Value -cnotmatch "^[A-Za-z0-9._-]+$") {
        throw "$Description must contain only ASCII letters, digits, dot, underscore, or hyphen."
    }
}

function Parse-UInt64 {
    param(
        [string]$Value,
        [string]$Description
    )

    [UInt64]$parsed = 0
    if (-not [UInt64]::TryParse($Value, $unsignedStyle, $invariant, [ref]$parsed)) {
        throw "$Description is not a canonical unsigned integer."
    }
    return $parsed
}

function Parse-Int64 {
    param(
        [string]$Value,
        [string]$Description
    )

    [Int64]$parsed = 0
    if (-not [Int64]::TryParse($Value, $integerStyle, $invariant, [ref]$parsed)) {
        throw "$Description is not a signed integer."
    }
    return $parsed
}

function Parse-Int32 {
    param(
        [string]$Value,
        [string]$Description
    )

    [Int32]$parsed = 0
    if (-not [Int32]::TryParse($Value, $integerStyle, $invariant, [ref]$parsed)) {
        throw "$Description is not a 32-bit signed integer."
    }
    return $parsed
}

function Get-NearestRankPercentile {
    param(
        [UInt64[]]$Values,
        [ValidateRange(0.01, 1.0)]
        [double]$Percentile
    )

    if ($Values.Count -eq 0) {
        return $null
    }
    $sorted = @($Values | Sort-Object)
    $index = [Math]::Max(0, [Math]::Ceiling($Percentile * $sorted.Count) - 1)
    return [UInt64]$sorted[$index]
}

function Read-ScenarioMatrix {
    param([string]$Path)

    $lines = [System.IO.File]::ReadAllLines($Path, [System.Text.Encoding]::UTF8)
    if ($lines.Count -lt 2 -or $lines[0] -cne $scenarioHeader) {
        throw "Scenario matrix must contain the exact four-column header and at least one row."
    }

    $keys = [System.Collections.Generic.HashSet[string]]::new($ordinal)
    $rows = [System.Collections.Generic.List[object]]::new()
    for ($lineIndex = 1; $lineIndex -lt $lines.Count; $lineIndex++) {
        $lineNumber = $lineIndex + 1
        if ([string]::IsNullOrWhiteSpace($lines[$lineIndex])) {
            throw "Scenario matrix line $lineNumber is blank."
        }
        $fields = $lines[$lineIndex].Split([char]9)
        if ($fields.Count -ne 4) {
            throw "Scenario matrix line $lineNumber has $($fields.Count) fields; expected 4."
        }

        Assert-SafeLabel $fields[0] "Scenario matrix line $lineNumber scenario"
        Assert-SafeLabel $fields[2] "Scenario matrix line $lineNumber layout"
        if ($fields[1] -cnotin @("resolve", "fail_closed")) {
            throw "Scenario matrix line $lineNumber has an invalid expectation."
        }
        if ([string]::IsNullOrWhiteSpace($fields[3])) {
            throw "Scenario matrix line $lineNumber must explain the observation."
        }

        $key = "$($fields[0])|$($fields[1])|$($fields[2])"
        if (-not $keys.Add($key)) {
            throw "Scenario matrix line $lineNumber duplicates scenario/expectation/layout $key."
        }
        $rows.Add([pscustomobject]@{
            Scenario = $fields[0]
            Expectation = $fields[1]
            Layout = $fields[2]
        })
    }
    return $rows
}

function Read-ResolverResults {
    param(
        [string[]]$Paths,
        [System.Collections.Generic.HashSet[string]]$ObservationKeys
    )

    $allRows = [System.Collections.Generic.List[object]]::new()
    foreach ($path in $Paths) {
        $lines = [System.IO.File]::ReadAllLines($path, [System.Text.Encoding]::UTF8)
        if ($lines.Count -lt 2 -or $lines[0] -cne $resolverHeader) {
            throw "Resolver result $path must contain the exact 17-column header and at least one row."
        }

        $caseIds = [System.Collections.Generic.HashSet[UInt64]]::new()
        for ($lineIndex = 1; $lineIndex -lt $lines.Count; $lineIndex++) {
            $lineNumber = $lineIndex + 1
            if ([string]::IsNullOrWhiteSpace($lines[$lineIndex])) {
                throw "Resolver result $path line $lineNumber is blank."
            }
            $fields = $lines[$lineIndex].Split([char]9)
            if ($fields.Count -ne 17) {
                throw "Resolver result $path line $lineNumber has $($fields.Count) fields; expected 17."
            }

            $caseId = Parse-UInt64 $fields[0] "Resolver result $path line $lineNumber case_id"
            if (-not $caseIds.Add($caseId)) {
                throw "Resolver result $path line $lineNumber duplicates case_id $caseId in that session."
            }

            $os = $fields[1]
            if ($os -cnotin $requiredOperatingSystems) {
                throw "Resolver result $path line $lineNumber has an unsupported os label."
            }
            Assert-SafeLabel $fields[2] "Resolver result $path line $lineNumber build"
            Assert-SafeLabel $fields[3] "Resolver result $path line $lineNumber dpi"
            Assert-SafeLabel $fields[4] "Resolver result $path line $lineNumber layout"
            Assert-SafeLabel $fields[5] "Resolver result $path line $lineNumber scenario"
            $x = Parse-Int32 $fields[6] "Resolver result $path line $lineNumber x"
            $y = Parse-Int32 $fields[7] "Resolver result $path line $lineNumber y"

            $expectation = $fields[8]
            $expectedPath = $fields[9]
            if ($expectation -ceq "resolve") {
                if ($expectedPath -cnotmatch "^[A-Za-z]:\\") {
                    throw "Resolver result $path line $lineNumber lacks a drive-absolute expected path."
                }
            }
            elseif ($expectation -ceq "fail_closed") {
                if ($expectedPath.Length -ne 0) {
                    throw "Resolver result $path line $lineNumber must leave expected_path empty."
                }
            }
            else {
                throw "Resolver result $path line $lineNumber has an invalid expectation."
            }

            $status = $fields[10]
            if ($status -cnotin @(
                "resolved",
                "unsupported",
                "ambiguous",
                "unavailable",
                "timed_out"
            )) {
                throw "Resolver result $path line $lineNumber has an invalid actual_status."
            }
            $actualPath = $fields[11]
            if ($status -ceq "resolved") {
                if ($actualPath -cnotmatch "^[A-Za-z]:\\") {
                    throw "Resolver result $path line $lineNumber resolved without an absolute path."
                }
            }
            elseif ($actualPath.Length -ne 0) {
                throw "Resolver result $path line $lineNumber has a path for a non-resolved status."
            }

            $elapsedUs = $null
            if ($fields[12].Length -ne 0) {
                $elapsedUs = Parse-UInt64 $fields[12] "Resolver result $path line $lineNumber elapsed_us"
            }
            Assert-SafeLabel $fields[13] "Resolver result $path line $lineNumber reason"
            $contextA = Parse-Int64 $fields[14] "Resolver result $path line $lineNumber context_a"
            $contextB = Parse-Int64 $fields[15] "Resolver result $path line $lineNumber context_b"
            $runnerFailure = $fields[13].StartsWith("runner.", [System.StringComparison]::Ordinal)
            if ($null -eq $elapsedUs -and -not $runnerFailure) {
                throw "Resolver result $path line $lineNumber omits latency without a runner failure."
            }

            $recomputedVerdict = if ($expectation -ceq "resolve") {
                if ($status -ceq "resolved") {
                    if ($ordinalIgnoreCase.Equals($expectedPath, $actualPath)) {
                        "correct_positive"
                    }
                    else {
                        "wrong_path"
                    }
                }
                else {
                    "missed_positive"
                }
            }
            elseif ($status -ceq "resolved") {
                "wrong_path"
            }
            else {
                "correct_fail_closed"
            }
            if ($fields[16] -cne $recomputedVerdict) {
                throw "Resolver result $path line $lineNumber verdict does not match its raw fields."
            }

            $observationKey = @(
                $fields[1],
                $fields[2],
                $fields[3],
                $fields[4],
                $fields[5],
                $fields[6],
                $fields[7],
                $fields[8],
                $fields[9]
            ) -join $tab
            if (-not $ObservationKeys.Add($observationKey)) {
                throw "Resolver result $path line $lineNumber duplicates a labeled point observation."
            }

            $allRows.Add([pscustomobject]@{
                Source = $path
                CaseId = $caseId
                Os = $os
                Build = $fields[2]
                Dpi = $fields[3]
                Layout = $fields[4]
                Scenario = $fields[5]
                X = $x
                Y = $y
                Expectation = $expectation
                Status = $status
                ElapsedUs = $elapsedUs
                Reason = $fields[13]
                ContextA = $contextA
                ContextB = $contextB
                Verdict = $recomputedVerdict
                RunnerFailure = $runnerFailure
            })
        }
    }
    return $allRows
}

function Read-WindowEvidence {
    param([string]$Path)

    $lines = [System.IO.File]::ReadAllLines($Path, [System.Text.Encoding]::UTF8)
    if ($lines.Count -lt 2 -or $lines[0] -cne $windowHeader) {
        throw "Window evidence must contain the exact 15-column header and at least one row."
    }

    $caseIds = [System.Collections.Generic.HashSet[UInt64]]::new()
    $observationKeys = [System.Collections.Generic.HashSet[string]]::new($ordinal)
    $rows = [System.Collections.Generic.List[object]]::new()
    for ($lineIndex = 1; $lineIndex -lt $lines.Count; $lineIndex++) {
        $lineNumber = $lineIndex + 1
        if ([string]::IsNullOrWhiteSpace($lines[$lineIndex])) {
            throw "Window evidence line $lineNumber is blank."
        }
        $fields = $lines[$lineIndex].Split([char]9)
        if ($fields.Count -ne 15) {
            throw "Window evidence line $lineNumber has $($fields.Count) fields; expected 15."
        }

        $caseId = Parse-UInt64 $fields[0] "Window evidence line $lineNumber case_id"
        if (-not $caseIds.Add($caseId)) {
            throw "Window evidence line $lineNumber duplicates case_id $caseId."
        }
        if ($fields[1] -cnotin $requiredOperatingSystems) {
            throw "Window evidence line $lineNumber has an unsupported os label."
        }
        foreach ($fieldIndex in 2..6) {
            Assert-SafeLabel $fields[$fieldIndex] "Window evidence line $lineNumber field $fieldIndex"
        }
        if ($fields[6] -cnotin $requiredWindowInteractions) {
            throw "Window evidence line $lineNumber has an unsupported interaction."
        }
        foreach ($fieldIndex in 7, 8, 10, 11, 12) {
            if ($fields[$fieldIndex] -cnotin @("yes", "no")) {
                throw "Window evidence line $lineNumber has an invalid yes/no observation."
            }
        }
        if ($fields[6] -cin @("left_click", "right_click")) {
            if ($fields[9] -cnotin @("yes", "no")) {
                throw "Window evidence line $lineNumber must record click delivery as yes or no."
            }
        }
        elseif ($fields[9] -cne "n/a") {
            throw "Window evidence line $lineNumber must record click delivery as n/a."
        }
        $uiThreadMaxUs =
            Parse-UInt64 $fields[13] "Window evidence line $lineNumber ui_thread_max_us"
        if ([string]::IsNullOrWhiteSpace($fields[14])) {
            throw "Window evidence line $lineNumber must contain an operator note."
        }

        $observationKey = @($fields[1], $fields[2], $fields[3], $fields[4], $fields[5], $fields[6]) `
            -join $tab
        if (-not $observationKeys.Add($observationKey)) {
            throw "Window evidence line $lineNumber duplicates an environment/scenario/interaction."
        }

        $rows.Add([pscustomobject]@{
            CaseId = $caseId
            Os = $fields[1]
            Build = $fields[2]
            Dpi = $fields[3]
            Layout = $fields[4]
            Scenario = $fields[5]
            Interaction = $fields[6]
            FocusPreserved = $fields[7]
            MouseActivationEaten = $fields[8]
            ClickDelivered = $fields[9]
            Dismissed = $fields[10]
            InsideWorkArea = $fields[11]
            PointerGapPreserved = $fields[12]
            UiThreadMaxUs = $uiThreadMaxUs
            Notes = $fields[14]
        })
    }
    return $rows
}

function Get-SourceDescription {
    param([string]$Path)

    $hash = Get-Sha256Hex -LiteralPath $Path
    return "- ``$([System.IO.Path]::GetFileName($Path))`` - SHA-256 ``$hash``"
}

function Join-MarkdownValues {
    param([object[]]$Values)

    if ($Values.Count -eq 0) {
        return "none"
    }
    return (@($Values | ForEach-Object { "``$_``" }) -join ", ")
}

$scenarioPath = Resolve-OneFile $ScenarioMatrix "Scenario matrix"
$resultPaths = [System.Collections.Generic.List[string]]::new()
if ($hasResolverDirectory) {
    $resolvedDirectory = @(Resolve-Path -LiteralPath $ResolverResultsDirectory -ErrorAction Stop)
    if ($resolvedDirectory.Count -ne 1 -or
        -not [System.IO.Directory]::Exists($resolvedDirectory[0].Path)) {
        throw "ResolverResultsDirectory must resolve to exactly one existing directory."
    }
    $directoryResults = @(
        [System.IO.Directory]::GetFiles(
            $resolvedDirectory[0].Path,
            "*.results.tsv",
            [System.IO.SearchOption]::TopDirectoryOnly
        ) | Sort-Object
    )
    if ($directoryResults.Count -eq 0) {
        throw "ResolverResultsDirectory contains no *.results.tsv files."
    }
    foreach ($candidate in $directoryResults) {
        $resultPaths.Add((Resolve-OneFile $candidate "Resolver result"))
    }
}
else {
    foreach ($candidate in $ResolverResults) {
        $resultPaths.Add((Resolve-OneFile $candidate "Resolver result"))
    }
}
if ($resultPaths.Count -eq 0) {
    throw "At least one resolver result file is required."
}
$windowPath = Resolve-OneFile $WindowEvidence "Window evidence"

$scenarioRequirements = @(Read-ScenarioMatrix $scenarioPath)
$catalogKeys = [System.Collections.Generic.HashSet[string]]::new($ordinal)
foreach ($requirement in $scenarioRequirements) {
    $catalogKeys.Add(
        "$($requirement.Scenario)|$($requirement.Expectation)|$($requirement.Layout)"
    ) | Out-Null
}
foreach ($requiredKey in @(
    $requiredCoreResolverKeys +
    $requiredWindows11ResolverKeys +
    $requiredTopologyResolverKeys
)) {
    if (-not $catalogKeys.Contains($requiredKey)) {
        throw "Scenario matrix does not define required qualification case $requiredKey."
    }
}
$resolverObservationKeys = [System.Collections.Generic.HashSet[string]]::new($ordinal)
$resolverRows = @(Read-ResolverResults $resultPaths $resolverObservationKeys)
$windowRows = @(Read-WindowEvidence $windowPath)

Write-Host "Validated $($resolverRows.Count) resolver rows from $($resultPaths.Count) files."
Write-Host "Validated $($windowRows.Count) preview-window evidence rows."
if ($ValidateOnly) {
    return
}

$positiveRows = @($resolverRows | Where-Object { $_.Expectation -ceq "resolve" })
$correctPositiveRows = @($resolverRows | Where-Object { $_.Verdict -ceq "correct_positive" })
$missedPositiveRows = @($resolverRows | Where-Object { $_.Verdict -ceq "missed_positive" })
$negativeRows = @($resolverRows | Where-Object { $_.Expectation -ceq "fail_closed" })
$wrongPathRows = @($resolverRows | Where-Object { $_.Verdict -ceq "wrong_path" })
$runnerFailureRows = @($resolverRows | Where-Object { $_.RunnerFailure })
$durations = [UInt64[]]@(
    $resolverRows |
        Where-Object { $null -ne $_.ElapsedUs } |
        ForEach-Object { $_.ElapsedUs }
)
$p50 = Get-NearestRankPercentile $durations 0.50
$p95 = Get-NearestRankPercentile $durations 0.95
$p99 = Get-NearestRankPercentile $durations 0.99
$coverage = if ($positiveRows.Count -eq 0) {
    0.0
}
else {
    100.0 * $correctPositiveRows.Count / $positiveRows.Count
}

$gateFailures = [System.Collections.Generic.List[string]]::new()
if ($resolverRows.Count -lt 2000) {
    $gateFailures.Add("resolver corpus contains fewer than 2,000 independent labeled rows")
}
if ($positiveRows.Count -eq 0 -or $coverage -lt 99.9) {
    $gateFailures.Add("resolver positive mapping coverage is below 99.9%")
}
if ($wrongPathRows.Count -ne 0) {
    $gateFailures.Add("resolver returned one or more wrong filesystem paths")
}
if ($runnerFailureRows.Count -ne 0) {
    $gateFailures.Add("resolver evidence contains one or more runner failures")
}
if ($null -eq $p95 -or $p95 -gt $resolverLatencyBudgetUs) {
    $gateFailures.Add("resolver p95 exceeds 50 ms or is unavailable")
}

$missingResolverCoverage = [System.Collections.Generic.List[string]]::new()
foreach ($os in $requiredOperatingSystems) {
    $osRows = @($resolverRows | Where-Object { $_.Os -ceq $os })
    if ($osRows.Count -eq 0) {
        $gateFailures.Add("resolver evidence does not include $os")
        continue
    }
    $requiredKeys = @($requiredCoreResolverKeys)
    if ($os -ceq "windows11") {
        $requiredKeys += $requiredWindows11ResolverKeys
    }
    foreach ($requiredKey in $requiredKeys) {
        $keyFields = $requiredKey.Split("|")
        $matching = @(
            $osRows | Where-Object {
                $_.Scenario -ceq $keyFields[0] -and
                $_.Expectation -ceq $keyFields[1] -and
                ($keyFields[2] -ceq "all" -or $_.Layout -ceq $keyFields[2])
            }
        )
        if ($matching.Count -eq 0) {
            $missingResolverCoverage.Add(
                "$os scenario=$($keyFields[0]) layout=$($keyFields[2])"
            )
        }
    }
}
foreach ($dpi in $requiredDpiValues) {
    if (@($resolverRows | Where-Object { $_.Dpi -ceq $dpi }).Count -eq 0) {
        $missingResolverCoverage.Add("aggregate dpi=$dpi")
    }
}
foreach ($requiredKey in $requiredTopologyResolverKeys) {
    $keyFields = $requiredKey.Split("|")
    $matching = @(
        $resolverRows | Where-Object {
            $_.Scenario -ceq $keyFields[0] -and
            $_.Expectation -ceq $keyFields[1] -and
            $_.Layout -ceq $keyFields[2]
        }
    )
    if ($matching.Count -eq 0) {
        $missingResolverCoverage.Add(
            "aggregate scenario=$($keyFields[0]) layout=$($keyFields[2])"
        )
    }
}
if ($missingResolverCoverage.Count -ne 0) {
    $gateFailures.Add("resolver OS/core-scenario/DPI/topology coverage is incomplete")
}

$windowFailureRows = @(
    $windowRows | Where-Object {
        $_.FocusPreserved -cne "yes" -or
        $_.MouseActivationEaten -cne "yes" -or
        $_.Dismissed -cne "yes" -or
        $_.InsideWorkArea -cne "yes" -or
        $_.PointerGapPreserved -cne "yes" -or
        $_.UiThreadMaxUs -gt $previewInitialTaskBudgetUs -or
        ($_.Interaction -cin @("left_click", "right_click") -and
            $_.ClickDelivered -cne "yes")
    }
)
if ($windowFailureRows.Count -ne 0) {
    $gateFailures.Add("preview-window evidence contains a failed focus/click/placement/task bound")
}

$missingWindowCoverage = [System.Collections.Generic.List[string]]::new()
foreach ($os in $requiredOperatingSystems) {
    $osRows = @($windowRows | Where-Object { $_.Os -ceq $os })
    if ($osRows.Count -eq 0) {
        $gateFailures.Add("preview-window evidence does not include $os")
        continue
    }
    foreach ($scenario in $requiredPerOsWindowScenarios) {
        if (@($osRows | Where-Object { $_.Scenario -ceq $scenario }).Count -eq 0) {
            $missingWindowCoverage.Add("$os scenario=$scenario")
        }
    }
    foreach ($interaction in $requiredWindowInteractions) {
        if (@($osRows | Where-Object { $_.Interaction -ceq $interaction }).Count -eq 0) {
            $missingWindowCoverage.Add("$os interaction=$interaction")
        }
    }
}
foreach ($dpi in $requiredDpiValues) {
    if (@($windowRows | Where-Object { $_.Dpi -ceq $dpi }).Count -eq 0) {
        $missingWindowCoverage.Add("aggregate dpi=$dpi")
    }
}
foreach ($scenario in $requiredAggregateWindowScenarios) {
    if (@($windowRows | Where-Object { $_.Scenario -ceq $scenario }).Count -eq 0) {
        $missingWindowCoverage.Add("aggregate scenario=$scenario")
    }
}
if ($missingWindowCoverage.Count -ne 0) {
    $gateFailures.Add("preview-window OS/scenario/interaction/DPI/topology coverage is incomplete")
}

if ([string]::IsNullOrWhiteSpace($Report)) {
    $reportDirectory = Join-Path ([System.IO.Path]::GetFullPath(
        (Join-Path $PSScriptRoot "..\target\qualification")
    )) "reports"
    [System.IO.Directory]::CreateDirectory($reportDirectory) | Out-Null
    $Report = Join-Path $reportDirectory "windows-release.md"
}
$reportPath = [System.IO.Path]::GetFullPath($Report)
$reportParent = [System.IO.Path]::GetDirectoryName($reportPath)
if (-not [string]::IsNullOrEmpty($reportParent)) {
    [System.IO.Directory]::CreateDirectory($reportParent) | Out-Null
}

$resolverOsSummary = @(
    foreach ($os in $requiredOperatingSystems) {
        $rows = @($resolverRows | Where-Object { $_.Os -ceq $os })
        if ($rows.Count -ne 0) {
            $builds = @($rows | Select-Object -ExpandProperty Build -Unique | Sort-Object)
            "| $os | $($rows.Count) | $(Join-MarkdownValues $builds) |"
        }
    }
)
$windowOsSummary = @(
    foreach ($os in $requiredOperatingSystems) {
        $rows = @($windowRows | Where-Object { $_.Os -ceq $os })
        if ($rows.Count -ne 0) {
            $maximum = ($rows | Measure-Object -Property UiThreadMaxUs -Maximum).Maximum
            "| $os | $($rows.Count) | $maximum |"
        }
    }
)
$sourceDescriptions = @(
    $resultPaths | ForEach-Object { Get-SourceDescription $_ }
    Get-SourceDescription $windowPath
    Get-SourceDescription $scenarioPath
)
$failureDescriptions = if ($gateFailures.Count -eq 0) {
    @("- None.")
}
else {
    @($gateFailures | ForEach-Object { "- $_." })
}
$missingResolverDescriptions = if ($missingResolverCoverage.Count -eq 0) {
    @("- None.")
}
else {
    @($missingResolverCoverage | ForEach-Object { "- ``$_``" })
}
$missingWindowDescriptions = if ($missingWindowCoverage.Count -eq 0) {
    @("- None.")
}
else {
    @($missingWindowCoverage | ForEach-Object { "- ``$_``" })
}
$gateStatus = if ($gateFailures.Count -eq 0) { "PASS" } else { "FAIL" }
$formatCoverage = $coverage.ToString("F3", $invariant)
$formatP50 = if ($null -eq $p50) { "n/a" } else { $p50 }
$formatP95 = if ($null -eq $p95) { "n/a" } else { $p95 }
$formatP99 = if ($null -eq $p99) { "n/a" } else { $p99 }
$reportLines = @(
    "# Windows release qualification",
    "",
    "> Gate result: **$gateStatus**",
    "",
    "This report is recomputed from strict raw TSV evidence. It is not a substitute for the raw",
    "sessions, operator notes, VM snapshots, or an independent review of how points were labeled.",
    "",
    "Coverage is risk-based rather than a Cartesian product. Core Explorer behavior and every",
    "interaction are required on each supported OS. The five DPI values, all work-area corners,",
    "negative coordinates, and mixed-DPI transitions are required across the combined matrix.",
    "",
    "## Resolver",
    "",
    "| Metric | Result | Required |",
    "|---|---:|---:|",
    "| Independent labeled rows | $($resolverRows.Count) | >=2,000 |",
    "| Supported positive rows | $($positiveRows.Count) | >0 |",
    "| Correct positive rows | $($correctPositiveRows.Count) | - |",
    "| Missed positive rows | $($missedPositiveRows.Count) | - |",
    "| Fail-closed rows | $($negativeRows.Count) | - |",
    "| Wrong paths | $($wrongPathRows.Count) | 0 |",
    "| Runner failures | $($runnerFailureRows.Count) | 0 |",
    "| Positive coverage | $formatCoverage% | >=99.900% |",
    "| Latency p50 | $formatP50 us | - |",
    "| Latency p95 | $formatP95 us | <=$resolverLatencyBudgetUs us |",
    "| Latency p99 | $formatP99 us | - |",
    "",
    "| OS | Rows | Builds |",
    "|---|---:|---|",
    $resolverOsSummary,
    "",
    "## Preview window",
    "",
    "| OS | Rows | Maximum preview create/show UI-thread task |",
    "|---|---:|---:|",
    $windowOsSummary,
    "",
    "Failed focus/click/placement/initial-task-bound rows: $($windowFailureRows.Count).",
    "",
    "The preview create/show task ceiling is $previewInitialTaskBudgetUs us. The separate idle",
    "performance gate retains the 16,000 us steady-state UI-thread ceiling.",
    "",
    "## Missing resolver coverage",
    "",
    $missingResolverDescriptions,
    "",
    "## Missing preview-window coverage",
    "",
    $missingWindowDescriptions,
    "",
    "## Gate failures",
    "",
    $failureDescriptions,
    "",
    "## Evidence files",
    "",
    $sourceDescriptions,
    "",
    "The report intentionally records file names and hashes, not machine-specific absolute paths."
) | ForEach-Object {
    if ($_ -is [System.Array]) {
        $_
    }
    else {
        [string]$_
    }
}

$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllLines($reportPath, [string[]]$reportLines, $utf8WithoutBom)
Write-Host "Windows qualification report: $reportPath"

if ($gateFailures.Count -ne 0) {
    throw "Windows qualification failed: $($gateFailures -join "; ")."
}
Write-Host "Windows resolver and preview-window release qualification passed."
