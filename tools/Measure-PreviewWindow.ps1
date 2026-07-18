[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [UInt64]$CaseId,

    [Parameter(Mandatory = $true)]
    [ValidateSet("windows10", "windows11")]
    [string]$Os,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Build,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Dpi,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Layout,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Scenario,

    [Parameter(Mandatory = $true)]
    [ValidateSet("timeout", "move", "wheel", "left_click", "right_click")]
    [string]$Interaction,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Notes,

    [ValidateSet("prompt", "yes", "no")]
    [string]$ClickDelivered = "prompt",

    [string]$Results,

    [ValidateRange(0, 30)]
    [int]$ActivationDelaySeconds = 5,

    [switch]$Practice,

    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($Results)) {
    if ($Practice) {
        $Results = Join-Path $PSScriptRoot `
            "..\target\qualification\evidence\window-attempts.tsv"
    }
    else {
        $Results = Join-Path $PSScriptRoot "..\target\qualification\evidence\window.tsv"
    }
}

$tab = "`t"
$header = @(
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

function Assert-SafeLabel {
    param(
        [string]$Value,
        [string]$Description
    )

    if ($Value -cnotmatch "^[A-Za-z0-9._-]+$") {
        throw "$Description must contain only ASCII letters, digits, dot, underscore, or hyphen."
    }
}

foreach ($label in @(
    @{ Value = $Build; Description = "Build" },
    @{ Value = $Dpi; Description = "Dpi" },
    @{ Value = $Layout; Description = "Layout" },
    @{ Value = $Scenario; Description = "Scenario" }
)) {
    Assert-SafeLabel $label.Value $label.Description
}
if ($Notes.IndexOfAny([char[]]@("`t", "`r", "`n")) -ge 0) {
    throw "Notes must fit on one TSV line and cannot contain tabs."
}
if ($Practice -and -not $Notes.StartsWith("practice_5s_", [System.StringComparison]::Ordinal)) {
    $Notes = "practice_5s_$Notes"
}

$resultPath = [System.IO.Path]::GetFullPath($Results)
if ($Practice -and
    [System.IO.Path]::GetFileName($resultPath).IndexOf(
        "attempt",
        [System.StringComparison]::OrdinalIgnoreCase
    ) -lt 0) {
    throw "Practice observations must be written to a clearly named attempts file."
}

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$targetDirectory = Join-Path $repoRoot "target\qualification\preview-window"
$executable = Join-Path $targetDirectory "release\CursorPeek.exe"
if (-not $SkipBuild) {
    & cargo build `
        --manifest-path (Join-Path $repoRoot "Cargo.toml") `
        --locked `
        --release `
        --target-dir $targetDirectory
    if ($LASTEXITCODE -ne 0) {
        throw "The preview-window diagnostic build failed."
    }
}
if (-not [System.IO.File]::Exists($executable)) {
    throw "Preview-window diagnostic executable not found: $executable"
}

$resultParent = [System.IO.Path]::GetDirectoryName($resultPath)
if (-not [string]::IsNullOrEmpty($resultParent)) {
    [System.IO.Directory]::CreateDirectory($resultParent) | Out-Null
}
if ([System.IO.File]::Exists($resultPath)) {
    $existingLines =
        [System.IO.File]::ReadAllLines($resultPath, [System.Text.Encoding]::UTF8)
    if ($existingLines.Count -eq 0 -or $existingLines[0] -cne $header) {
        throw "Existing window evidence does not use the required 15-column schema."
    }
    for ($lineIndex = 1; $lineIndex -lt $existingLines.Count; $lineIndex++) {
        $fields = $existingLines[$lineIndex].Split([char]9)
        if ($fields.Count -ne 15) {
            throw "Existing window evidence line $($lineIndex + 1) is malformed."
        }
        [UInt64]$existingCaseId = 0
        if (-not [UInt64]::TryParse($fields[0], [ref]$existingCaseId)) {
            throw "Existing window evidence line $($lineIndex + 1) has an invalid case ID."
        }
        if ($existingCaseId -eq $CaseId) {
            throw "Existing window evidence already contains case_id $CaseId."
        }
    }
}

$instruction = switch ($Interaction) {
    "timeout" {
        "Keep the pointer stationary and do not click until the blue rectangle disappears."
    }
    "move" {
        "Move the pointer once after the blue rectangle appears."
    }
    "wheel" {
        "Turn the mouse wheel once after the blue rectangle appears."
    }
    "left_click" {
        "Left-click the prepared Explorer target once after the blue rectangle appears."
    }
    "right_click" {
        "Right-click the prepared Explorer target once after the blue rectangle appears."
    }
}
Write-Host $instruction
$observationDuration = if ($Practice) { "5-second practice" } else { "1.5-second qualification" }
Write-Host "Place the pointer at the labeled point. The $observationDuration observation starts in $ActivationDelaySeconds seconds."
if ($ActivationDelaySeconds -gt 0) {
    Start-Sleep -Seconds $ActivationDelaySeconds
}

$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $executable
$startInfo.Arguments = if ($Practice) {
    "--preview-window-practice-diagnostics"
}
else {
    "--preview-window-diagnostics"
}
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true

$process = [System.Diagnostics.Process]::new()
$process.StartInfo = $startInfo
try {
    if (-not $process.Start()) {
        throw "Failed to start the preview-window diagnostic."
    }
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) {
        throw "Preview-window diagnostic exited with code $($process.ExitCode): $stderr"
    }
}
finally {
    $process.Dispose()
}

$line = $stdout.Trim()
$pattern = "^No-activate preview diagnostic completed: " +
    "focus_preserved=(yes|no), mouse_activation=(eaten|passed), " +
    "dismissal=(input|timeout|shutdown), x=(-?[0-9]+), y=(-?[0-9]+), " +
    "width=([0-9]+), height=([0-9]+), inside_work_area=(yes|no), " +
    "pointer_gap_preserved=(yes|no), ui_thread_max_us=([0-9]+)$"
$match = [regex]::Match($line, $pattern, [System.Text.RegularExpressions.RegexOptions]::CultureInvariant)
if (-not $match.Success) {
    throw "Preview-window diagnostic returned an unexpected result: $line"
}

$focusPreserved = $match.Groups[1].Value
$mouseActivationEaten = if ($match.Groups[2].Value -ceq "eaten") { "yes" } else { "no" }
$dismissal = $match.Groups[3].Value
$insideWorkArea = $match.Groups[8].Value
$pointerGapPreserved = $match.Groups[9].Value
$uiThreadMaxUs = $match.Groups[10].Value
$expectedDismissal = if ($Interaction -ceq "timeout") { "timeout" } else { "input" }
$dismissed = if ($dismissal -ceq $expectedDismissal) { "yes" } else { "no" }

$clickObservation = "n/a"
if ($Interaction -cin @("left_click", "right_click")) {
    if ($ClickDelivered -ceq "prompt") {
        $observed = Read-Host "Did Explorer receive the intended click exactly once? Type yes or no"
        if ($observed -cnotin @("yes", "no")) {
            throw "Click delivery must be recorded explicitly as yes or no."
        }
        $clickObservation = $observed
    }
    else {
        $clickObservation = $ClickDelivered
    }
}
elseif ($ClickDelivered -cne "prompt") {
    throw "ClickDelivered applies only to left_click or right_click observations."
}

$row = @(
    $CaseId,
    $Os,
    $Build,
    $Dpi,
    $Layout,
    $Scenario,
    $Interaction,
    $focusPreserved,
    $mouseActivationEaten,
    $clickObservation,
    $dismissed,
    $insideWorkArea,
    $pointerGapPreserved,
    $uiThreadMaxUs,
    $Notes
) -join $tab

$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
if (-not [System.IO.File]::Exists($resultPath)) {
    $writer = [System.IO.StreamWriter]::new($resultPath, $false, $utf8WithoutBom)
}
else {
    $writer = [System.IO.StreamWriter]::new($resultPath, $true, $utf8WithoutBom)
}
try {
    if ($writer.BaseStream.Length -eq 0) {
        $writer.WriteLine($header)
    }
    $writer.WriteLine($row)
}
finally {
    $writer.Dispose()
}

Write-Host $line
$recordKind = if ($Practice) { "practice observation" } else { "preview-window evidence" }
Write-Host "Recorded $recordKind case $CaseId in $resultPath"
if ($focusPreserved -cne "yes" -or
    $mouseActivationEaten -cne "yes" -or
    $clickObservation -ceq "no" -or
    $dismissed -cne "yes" -or
    $insideWorkArea -cne "yes" -or
    $pointerGapPreserved -cne "yes" -or
    [UInt64]$uiThreadMaxUs -gt 16000) {
    throw "The observation was preserved, but it does not satisfy the preview-window gate."
}
