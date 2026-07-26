[CmdletBinding()]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $Executable = 'target/release/CursorPeek.exe',

    [Parameter()]
    [ValidateRange(1, 10)]
    [int] $Runs = 3,

    [Parameter()]
    [ValidateRange(10, 300)]
    [int] $TimeoutSeconds = 120
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
if ([System.IO.Path]::GetFileName($resolvedExecutable) -cne 'CursorPeek.exe') {
    throw "The recovery soak requires CursorPeek.exe, not '$resolvedExecutable'."
}

function Get-CursorPeekWorkerProcesses {
    $matches = @(
        Get-CimInstance -ClassName Win32_Process -Filter "Name = 'CursorPeek.exe'" |
            Where-Object {
                -not [string]::IsNullOrWhiteSpace($_.ExecutablePath) -and
                [string]::Equals(
                    [System.IO.Path]::GetFullPath($_.ExecutablePath),
                    $resolvedExecutable,
                    [System.StringComparison]::OrdinalIgnoreCase
                ) -and
                $_.CommandLine -match '(?i)(?:^|\s)--preview-worker(?:\s|$)'
            }
    )
    return @($matches | Sort-Object ProcessId)
}

$preexistingWorkers = @(Get-CursorPeekWorkerProcesses)
if ($preexistingWorkers.Count -ne 0) {
    $identifiers = ($preexistingWorkers | ForEach-Object ProcessId) -join ', '
    throw "Close the existing CursorPeek worker processes before the soak: $identifiers."
}

$expectedReport = '^Recovery soak completed: cycles=32, requests=132, sessions=69, ' +
    'taskbar_recoveries=32, power_cycles=32, idle_restarts=4, forced_timeouts=4, ' +
    'residual_workers=0, elapsed=([0-9]+) ms$'
$maximumReportedMilliseconds = 0L
$maximumWallMilliseconds = 0L

for ($run = 1; $run -le $Runs; $run++) {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $resolvedExecutable
    $startInfo.Arguments = '--recovery-soak-diagnostics'
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $wall = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        if (-not $process.Start()) {
            throw "Recovery soak run $run did not start."
        }
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill()
            if (-not $process.WaitForExit(5000)) {
                throw "Recovery soak run $run did not exit within five seconds after termination."
            }
            throw "Recovery soak run $run exceeded $TimeoutSeconds seconds."
        }
        $wall.Stop()
        $stdout = $process.StandardOutput.ReadToEnd().Trim()
        $stderr = $process.StandardError.ReadToEnd().Trim()
        if ($process.ExitCode -ne 0) {
            throw "Recovery soak run $run exited with code $($process.ExitCode): $stderr"
        }
        if (-not [string]::IsNullOrWhiteSpace($stderr)) {
            throw "Recovery soak run $run wrote unexpected stderr: $stderr"
        }
        if ($stdout -notmatch $expectedReport) {
            throw "Recovery soak run $run returned an unexpected report: $stdout"
        }

        $reportedMilliseconds = [long] $Matches[1]
        $maximumReportedMilliseconds = [Math]::Max(
            $maximumReportedMilliseconds,
            $reportedMilliseconds
        )
        $maximumWallMilliseconds = [Math]::Max(
            $maximumWallMilliseconds,
            $wall.ElapsedMilliseconds
        )
    }
    finally {
        $wall.Stop()
        $process.Dispose()
    }

    $cleanupDeadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        $remainingWorkers = @(Get-CursorPeekWorkerProcesses)
        if ($remainingWorkers.Count -eq 0) {
            break
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $cleanupDeadline)

    if ($remainingWorkers.Count -ne 0) {
        $identifiers = ($remainingWorkers | ForEach-Object ProcessId) -join ', '
        throw "Recovery soak run $run left worker processes behind: $identifiers."
    }
}

(
    "Recovery soak gate passed: runs={0}, cycles_per_run=32, residual_workers=0, " +
    "max_reported_ms={1}, max_wall_ms={2}"
) -f $Runs, $maximumReportedMilliseconds, $maximumWallMilliseconds
