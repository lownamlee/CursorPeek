[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Manifest,

    [string]$Results,

    [ValidateRange(100, 10000)]
    [int]$TimeoutMilliseconds = 1250,

    [ValidateRange(0, 30)]
    [int]$ActivationDelaySeconds = 5,

    [switch]$Evaluate,
    [switch]$ValidateOnly,
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$manifestPath = (Resolve-Path -LiteralPath $Manifest).Path
$tab = "`t"
$expectedHeader = @(
    "case_id",
    "os",
    "build",
    "dpi",
    "layout",
    "scenario",
    "x",
    "y",
    "expectation",
    "expected_path"
) -join $tab

function Read-CorpusManifest {
    param([string]$Path)

    $lines = [System.IO.File]::ReadAllLines($Path, [System.Text.Encoding]::UTF8)
    if ($lines.Count -lt 2) {
        throw "Corpus manifest must contain the header and at least one labeled case."
    }
    if ($lines[0] -cne $expectedHeader) {
        throw "Corpus manifest header does not match the required ten-column schema."
    }

    $ids = [System.Collections.Generic.HashSet[UInt64]]::new()
    $cases = [System.Collections.Generic.List[object]]::new()

    for ($lineIndex = 1; $lineIndex -lt $lines.Count; $lineIndex++) {
        $lineNumber = $lineIndex + 1
        if ([string]::IsNullOrWhiteSpace($lines[$lineIndex])) {
            throw "Corpus manifest line $lineNumber is blank."
        }

        $fields = $lines[$lineIndex].Split([char]9)
        if ($fields.Count -ne 10) {
            throw "Corpus manifest line $lineNumber has $($fields.Count) fields; expected 10."
        }

        [UInt64]$caseId = 0
        if (-not [UInt64]::TryParse(
            $fields[0],
            [System.Globalization.NumberStyles]::None,
            [System.Globalization.CultureInfo]::InvariantCulture,
            [ref]$caseId
        )) {
            throw "Corpus manifest line $lineNumber has an invalid case_id."
        }
        if (-not $ids.Add($caseId)) {
            throw "Corpus manifest line $lineNumber duplicates case_id $caseId."
        }

        $os = $fields[1]
        if ($os -cnotin @("windows10", "windows11")) {
            throw "Corpus manifest line $lineNumber must label os as windows10 or windows11."
        }
        foreach ($fieldIndex in 2..5) {
            if ([string]::IsNullOrWhiteSpace($fields[$fieldIndex])) {
                throw "Corpus manifest line $lineNumber has an empty environment/scenario label."
            }
        }

        [Int32]$x = 0
        [Int32]$y = 0
        $integerStyle = [System.Globalization.NumberStyles]::AllowLeadingSign
        $invariant = [System.Globalization.CultureInfo]::InvariantCulture
        if (-not [Int32]::TryParse($fields[6], $integerStyle, $invariant, [ref]$x)) {
            throw "Corpus manifest line $lineNumber has an invalid x coordinate."
        }
        if (-not [Int32]::TryParse($fields[7], $integerStyle, $invariant, [ref]$y)) {
            throw "Corpus manifest line $lineNumber has an invalid y coordinate."
        }

        $expectation = $fields[8]
        $expectedPath = $fields[9]
        if ($expectation -ceq "resolve") {
            if ($expectedPath -cnotmatch "^[A-Za-z]:\\") {
                throw "Corpus manifest line $lineNumber must provide a drive-absolute expected path."
            }
            if (-not [System.IO.File]::Exists($expectedPath)) {
                throw "Corpus manifest line $lineNumber expected file does not exist: $expectedPath"
            }
        }
        elseif ($expectation -ceq "fail_closed") {
            if ($expectedPath.Length -ne 0) {
                throw "Corpus manifest line $lineNumber must leave expected_path empty for fail_closed."
            }
        }
        else {
            throw "Corpus manifest line $lineNumber has an invalid expectation."
        }

        $cases.Add([pscustomobject]@{
            CaseId = $caseId
            Os = $os
            Build = $fields[2]
            Dpi = $fields[3]
            Layout = $fields[4]
            Scenario = $fields[5]
            X = $x
            Y = $y
            Expectation = $expectation
            ExpectedPath = $expectedPath
        })
    }

    return $cases
}

function Start-ResolverProbe {
    param([string]$Executable)

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.Arguments = "--resolver-corpus-probe"
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardInput = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        $process.Dispose()
        throw "Failed to start the resolver corpus probe."
    }
    return $process
}

function Stop-ResolverProbe {
    param([AllowNull()][System.Diagnostics.Process]$Process)

    if ($null -eq $Process) {
        return
    }
    try {
        if (-not $Process.HasExited) {
            $Process.Kill()
            [void]$Process.WaitForExit(2000)
        }
    }
    finally {
        $Process.Dispose()
    }
}

function ConvertFrom-Utf16Hex {
    param([string]$Hex)

    if ($Hex.Length -eq 0) {
        return ""
    }
    if ($Hex.Length -gt (32767 * 4) -or ($Hex.Length % 4) -ne 0 -or $Hex -cnotmatch "^[0-9A-F]+$") {
        throw "Probe returned malformed or oversized UTF-16 path data."
    }

    $unitCount = [int]($Hex.Length / 4)
    $bytes = [byte[]]::new($unitCount * 2)
    for ($index = 0; $index -lt $unitCount; $index++) {
        $unit = [Convert]::ToUInt16($Hex.Substring($index * 4, 4), 16)
        $bytes[$index * 2] = [byte]($unit -band 0xFF)
        $bytes[($index * 2) + 1] = [byte]($unit -shr 8)
    }
    return [System.Text.Encoding]::Unicode.GetString($bytes)
}

function Read-ProbeResult {
    param(
        [System.Diagnostics.Process]$Process,
        [UInt64]$ExpectedCaseId,
        [int]$DeadlineMilliseconds
    )

    $readTask = $Process.StandardOutput.ReadLineAsync()
    if (-not $readTask.Wait($DeadlineMilliseconds)) {
        return [pscustomobject]@{
            RunnerStatus = "timeout"
            Status = "timed_out"
            ElapsedUs = $null
            Path = ""
            Reason = "runner.deadline_exceeded"
            ContextA = 0
            ContextB = 0
        }
    }

    $line = $readTask.Result
    if ($null -eq $line) {
        $stderr = $Process.StandardError.ReadToEnd()
        return [pscustomobject]@{
            RunnerStatus = "crash"
            Status = "unavailable"
            ElapsedUs = $null
            Path = ""
            Reason = if ([string]::IsNullOrWhiteSpace($stderr)) {
                "runner.probe_exited"
            }
            else {
                "runner.probe_error"
            }
            ContextA = 0
            ContextB = 0
        }
    }
    if ($line.Length -gt 131300) {
        throw "Probe response exceeds the bounded result-line limit."
    }

    $fields = $line.Split([char]9)
    if ($fields.Count -ne 7) {
        throw "Probe response has $($fields.Count) fields; expected 7."
    }

    [UInt64]$caseId = 0
    if (-not [UInt64]::TryParse($fields[0], [ref]$caseId) -or $caseId -ne $ExpectedCaseId) {
        throw "Probe response case ID does not match the request."
    }
    if ($fields[1] -cnotin @("resolved", "unsupported", "ambiguous", "unavailable")) {
        throw "Probe response has an invalid resolver status."
    }

    [UInt64]$elapsedUs = 0
    if (-not [UInt64]::TryParse($fields[2], [ref]$elapsedUs)) {
        throw "Probe response has an invalid duration."
    }
    if ($fields[4] -cnotmatch "^[a-z0-9._]+$") {
        throw "Probe response has an invalid reason token."
    }

    [Int64]$contextA = 0
    [Int64]$contextB = 0
    if (-not [Int64]::TryParse($fields[5], [ref]$contextA) -or
        -not [Int64]::TryParse($fields[6], [ref]$contextB)) {
        throw "Probe response has invalid reason context."
    }

    $path = ConvertFrom-Utf16Hex $fields[3]
    if ($fields[1] -ceq "resolved" -and $path -cnotmatch "^[A-Za-z]:\\") {
        throw "Resolved probe response does not contain a drive-absolute path."
    }
    if ($fields[1] -cne "resolved" -and $path.Length -ne 0) {
        throw "Non-resolved probe response unexpectedly contains a path."
    }

    return [pscustomobject]@{
        RunnerStatus = "ok"
        Status = $fields[1]
        ElapsedUs = $elapsedUs
        Path = $path
        Reason = $fields[4]
        ContextA = $contextA
        ContextB = $contextB
    }
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

$cases = @(Read-CorpusManifest $manifestPath)
Write-Host "Validated $($cases.Count) labeled resolver cases."
if ($ValidateOnly) {
    return
}

$targetDirectory = Join-Path $repoRoot "target\resolver-corpus"
$executable = Join-Path $targetDirectory "release\CursorPeek.exe"
if (-not $SkipBuild) {
    & cargo build `
        --manifest-path (Join-Path $repoRoot "Cargo.toml") `
        --locked `
        --release `
        --features resolver-corpus `
        --target-dir $targetDirectory
    if ($LASTEXITCODE -ne 0) {
        throw "The resolver corpus probe build failed."
    }
}
if (-not [System.IO.File]::Exists($executable)) {
    throw "Resolver corpus probe executable not found: $executable"
}

if ([string]::IsNullOrWhiteSpace($Results)) {
    $resultDirectory = Join-Path $targetDirectory "results"
    [System.IO.Directory]::CreateDirectory($resultDirectory) | Out-Null
    $baseName = [System.IO.Path]::GetFileNameWithoutExtension($manifestPath)
    $Results = Join-Path $resultDirectory "$baseName.results.tsv"
}
$resultPath = [System.IO.Path]::GetFullPath($Results)
$resultParent = [System.IO.Path]::GetDirectoryName($resultPath)
if (-not [string]::IsNullOrEmpty($resultParent)) {
    [System.IO.Directory]::CreateDirectory($resultParent) | Out-Null
}

if ($ActivationDelaySeconds -gt 0) {
    Write-Host "Switch to the prepared Explorer window. Sampling starts in $ActivationDelaySeconds seconds."
    Start-Sleep -Seconds $ActivationDelaySeconds
}

$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$writer = [System.IO.StreamWriter]::new($resultPath, $false, $utf8WithoutBom)
$writer.WriteLine(@(
    $expectedHeader,
    "actual_status",
    "actual_path",
    "elapsed_us",
    "reason",
    "context_a",
    "context_b",
    "verdict"
) -join $tab)

$probe = $null
$rows = [System.Collections.Generic.List[object]]::new()
try {
    foreach ($case in $cases) {
        if ($null -eq $probe -or $probe.HasExited) {
            Stop-ResolverProbe $probe
            $probe = Start-ResolverProbe $executable
        }

        $requestLine = "{0}`t{1}`t{2}" -f $case.CaseId, $case.X, $case.Y
        $probe.StandardInput.WriteLine($requestLine)
        $probe.StandardInput.Flush()

        $actual = Read-ProbeResult $probe $case.CaseId $TimeoutMilliseconds
        if ($actual.RunnerStatus -cne "ok") {
            Stop-ResolverProbe $probe
            $probe = $null
        }

        $verdict = ""
        if ($case.Expectation -ceq "resolve") {
            if ($actual.Status -ceq "resolved") {
                if ([System.StringComparer]::OrdinalIgnoreCase.Equals(
                    $case.ExpectedPath,
                    $actual.Path
                )) {
                    $verdict = "correct_positive"
                }
                else {
                    $verdict = "wrong_path"
                }
            }
            else {
                $verdict = "missed_positive"
            }
        }
        elseif ($actual.Status -ceq "resolved") {
            $verdict = "wrong_path"
        }
        else {
            $verdict = "correct_fail_closed"
        }

        $row = [pscustomobject]@{
            Case = $case
            Actual = $actual
            Verdict = $verdict
        }
        $rows.Add($row)

        $writer.WriteLine(@(
            $case.CaseId,
            $case.Os,
            $case.Build,
            $case.Dpi,
            $case.Layout,
            $case.Scenario,
            $case.X,
            $case.Y,
            $case.Expectation,
            $case.ExpectedPath,
            $actual.Status,
            $actual.Path,
            $(if ($null -eq $actual.ElapsedUs) { "" } else { $actual.ElapsedUs }),
            $actual.Reason,
            $actual.ContextA,
            $actual.ContextB,
            $verdict
        ) -join $tab)
        $writer.Flush()
    }
}
finally {
    Stop-ResolverProbe $probe
    $writer.Dispose()
}

$positiveCount = @($rows | Where-Object { $_.Case.Expectation -ceq "resolve" }).Count
$correctPositive = @($rows | Where-Object { $_.Verdict -ceq "correct_positive" }).Count
$missedPositive = @($rows | Where-Object { $_.Verdict -ceq "missed_positive" }).Count
$negativeCount = $rows.Count - $positiveCount
$negativeResolutions = @(
    $rows | Where-Object {
        $_.Case.Expectation -ceq "fail_closed" -and $_.Actual.Status -ceq "resolved"
    }
).Count
$wrongPaths = @($rows | Where-Object { $_.Verdict -ceq "wrong_path" }).Count
$runnerFailures = @($rows | Where-Object { $_.Actual.RunnerStatus -cne "ok" }).Count
$durations = [UInt64[]]@(
    $rows |
        Where-Object { $null -ne $_.Actual.ElapsedUs } |
        ForEach-Object { $_.Actual.ElapsedUs }
)
$p50 = Get-NearestRankPercentile $durations 0.50
$p95 = Get-NearestRankPercentile $durations 0.95
$p99 = Get-NearestRankPercentile $durations 0.99
$coverage = if ($positiveCount -eq 0) {
    0.0
}
else {
    100.0 * $correctPositive / $positiveCount
}

Write-Host ""
Write-Host "Resolver corpus results: $resultPath"
$summaryFormat = "Cases={0}; positives={1}; correct={2}; missed={3}; negatives={4}; " +
    "negative_resolutions={5}; wrong_paths={6}; runner_failures={7}"
Write-Host ($summaryFormat -f
    $rows.Count,
    $positiveCount,
    $correctPositive,
    $missedPositive,
    $negativeCount,
    $negativeResolutions,
    $wrongPaths,
    $runnerFailures)
Write-Host ("Positive coverage={0:N3}%; latency_us p50={1}, p95={2}, p99={3}" -f
    $coverage,
    $(if ($null -eq $p50) { "n/a" } else { $p50 }),
    $(if ($null -eq $p95) { "n/a" } else { $p95 }),
    $(if ($null -eq $p99) { "n/a" } else { $p99 }))

$reasonGroups = $rows |
    Group-Object { "$($_.Actual.Status)|$($_.Actual.Reason)" } |
    Sort-Object -Property @(
        @{ Expression = "Count"; Descending = $true },
        @{ Expression = "Name"; Ascending = $true }
    )
foreach ($group in $reasonGroups) {
    Write-Host ("  {0}: {1}" -f $group.Name, $group.Count)
}

if ($Evaluate) {
    $osLabels = @($cases | Select-Object -ExpandProperty Os -Unique)
    $gateFailures = [System.Collections.Generic.List[string]]::new()
    if ($rows.Count -lt 2000) {
        $gateFailures.Add("fewer than 2,000 labeled cases")
    }
    if ($osLabels -cnotcontains "windows10" -or $osLabels -cnotcontains "windows11") {
        $gateFailures.Add("both Windows 10 and Windows 11 evidence are required")
    }
    if ($positiveCount -eq 0 -or $coverage -lt 99.9) {
        $gateFailures.Add("positive mapping coverage is below 99.9%")
    }
    if ($wrongPaths -ne 0) {
        $gateFailures.Add("one or more wrong paths were resolved")
    }
    if ($runnerFailures -ne 0) {
        $gateFailures.Add("one or more probe cases timed out or crashed")
    }
    if ($null -eq $p95 -or $p95 -gt 50000) {
        $gateFailures.Add("resolver p95 exceeds 50 ms or is unavailable")
    }
    if ($gateFailures.Count -ne 0) {
        throw "Resolver release gate failed: $($gateFailures -join "; ")."
    }
    Write-Host "Resolver release gate passed for this combined corpus."
}
