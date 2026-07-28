[CmdletBinding()]
param(
    [Parameter()]
    [ValidatePattern('^[A-Za-z0-9_-]{1,64}$')]
    [string] $RunId = '',

    [Parameter()]
    [switch] $Timeline
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$localAppData = [Environment]::GetFolderPath(
    [Environment+SpecialFolder]::LocalApplicationData
)
if ([string]::IsNullOrWhiteSpace($localAppData)) {
    throw 'The current user has no Local AppData directory.'
}

$root = Join-Path $localAppData 'CursorPeek\diagnostics'
if ([string]::IsNullOrWhiteSpace($RunId)) {
    $latestPath = Join-Path $root 'latest-run.txt'
    if (-not (Test-Path -LiteralPath $latestPath -PathType Leaf)) {
        throw "No CursorPeek diagnostic run was found at '$root'."
    }
    $RunId = (Get-Content -LiteralPath $latestPath -Raw).Trim()
    if ($RunId -notmatch '^[A-Za-z0-9_-]{1,64}$') {
        throw 'The latest diagnostic run identifier is invalid.'
    }
}

$runDirectory = Join-Path $root $RunId
$files = @(
    Get-ChildItem -LiteralPath $runDirectory -Filter '*.jsonl' -File |
        Sort-Object Name
)
if ($files.Count -eq 0) {
    throw "No JSONL logs were found in '$runDirectory'."
}

$records = [System.Collections.Generic.List[object]]::new()
foreach ($file in $files) {
    foreach ($line in Get-Content -LiteralPath $file.FullName) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        try {
            $record = $line | ConvertFrom-Json -ErrorAction Stop
            $record | Add-Member -NotePropertyName source_file -NotePropertyValue $file.Name
            $records.Add($record)
        }
        catch {
            Write-Warning "Ignored an incomplete JSONL line in '$($file.Name)'."
        }
    }
}

$ordered = @(
    $records | Sort-Object @{
        Expression = {
            if ($_.PSObject.Properties.Name -contains 'qpc') {
                [Int64] $_.qpc
            }
            else {
                [Int64]::MaxValue
            }
        }
    }, pid, tid
)

function Get-DetailValue {
    param(
        [Parameter(Mandatory = $true)][string] $Detail,
        [Parameter(Mandatory = $true)][string] $Name
    )

    $match = [regex]::Match($Detail, "(?:^| )$([regex]::Escape($Name))=([^ ]+)")
    if ($match.Success) {
        return $match.Groups[1].Value
    }
    return $null
}

function Get-Generation {
    param([Parameter(Mandatory = $true)][object] $Record)

    if ($null -eq $Record.detail) {
        return $null
    }
    $value = Get-DetailValue ([string] $Record.detail) 'generation'
    if ($null -eq $value) {
        return $null
    }
    return [UInt64] $value
}

$starts = @{}
$dwellCompleted = @{}
$managerCompleted = @{}
$rows = [System.Collections.Generic.List[object]]::new()

foreach ($record in $ordered) {
    $generation = Get-Generation $record
    if ($null -eq $generation) {
        continue
    }
    $key = [string] $generation
    switch ([string] $record.event) {
        'hover.dwell.started' {
            $starts[$key] = $record
        }
        'hover.dwell.completed' {
            $dwellCompleted[$key] = $record
        }
        'worker.manager.resolved' {
            $managerCompleted[$key] = $record
        }
        'preview.visible' {
            if (-not $starts.ContainsKey($key)) {
                continue
            }
            $start = $starts[$key]
            $frequency = [double] $record.qpc_frequency
            $endToEndMs = (([Int64] $record.qpc - [Int64] $start.qpc) * 1000.0) / $frequency
            $dwellMs = $null
            if ($dwellCompleted.ContainsKey($key)) {
                $dwellMs = (
                    ([Int64] $dwellCompleted[$key].qpc - [Int64] $start.qpc) * 1000.0
                ) / $frequency
            }
            $workerMs = $null
            if ($managerCompleted.ContainsKey($key)) {
                $workerUs = Get-DetailValue ([string] $managerCompleted[$key].detail) 'elapsed_us'
                if ($null -ne $workerUs) {
                    $workerMs = [double] $workerUs / 1000.0
                }
            }
            $showUs = Get-DetailValue ([string] $record.detail) 'show_us'
            $rows.Add([pscustomobject]@{
                Generation = $generation
                EndToEndMs = [Math]::Round($endToEndMs, 3)
                DwellMs = if ($null -eq $dwellMs) { $null } else {
                    [Math]::Round($dwellMs, 3)
                }
                WorkerMs = if ($null -eq $workerMs) { $null } else {
                    [Math]::Round($workerMs, 3)
                }
                ShowMs = if ($null -eq $showUs) { $null } else {
                    [Math]::Round(([double] $showUs / 1000.0), 3)
                }
                Kind = Get-DetailValue ([string] $record.detail) 'kind'
            })
        }
    }
}

$errors = @(
    $ordered | Where-Object {
        $_.detail -match '(?:^| )outcome=error(?: |$)' -or
        $_.event -in @('preview.show.failed', 'logger.size_limit')
    }
)
$summaries = @($ordered | Where-Object { $_.event -eq 'logger.summary' })

Write-Output "CursorPeek diagnostic run: $RunId"
Write-Output "Directory: $runDirectory"
Write-Output "Files: $($files.Count); events: $($ordered.Count); error events: $($errors.Count)"

if ($rows.Count -gt 0) {
    Write-Output ''
    Write-Output 'Completed preview latency:'
    $rows | Format-Table -AutoSize
}
else {
    Write-Output ''
    Write-Output 'No complete hover-to-visible preview sequence was found.'
}

if ($summaries.Count -gt 0) {
    Write-Output ''
    Write-Output 'Logger summaries:'
    $summaries | Select-Object role, pid, detail, source_file | Format-Table -AutoSize
}

if ($errors.Count -gt 0) {
    Write-Output ''
    Write-Output 'Error events:'
    $errors |
        Select-Object unix_ms, role, event, detail, source_file |
        Format-Table -Wrap -AutoSize
}

if ($Timeline) {
    Write-Output ''
    Write-Output 'Timeline:'
    $ordered |
        Select-Object unix_ms, qpc, pid, tid, role, event, detail, source_file |
        Format-Table -Wrap -AutoSize
}
