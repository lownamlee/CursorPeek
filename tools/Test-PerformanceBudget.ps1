[CmdletBinding()]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $Executable = 'target/release/CursorPeek.exe',

    [Parameter()]
    [ValidateRange(3, 20)]
    [int] $Runs = 5,

    [Parameter()]
    [ValidateRange(5, 120)]
    [int] $TimeoutSeconds = 20
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$maximumExecutableBytes = 4 * 1024 * 1024
$maximumStartupP95Microseconds = 750000L
$maximumWorkerP95Milliseconds = 1500L
$maximumIdleUiTaskMicroseconds = 16000L
$maximumCoordinatorWorkingSetBytes = 32L * 1024 * 1024
$maximumCoordinatorPrivateBytes = 16L * 1024 * 1024
$maximumCoordinatorHandles = 256
$maximumCoordinatorThreads = 16
$maximumWorkerWorkingSetBytes = 32L * 1024 * 1024
$maximumWorkerPrivateBytes = 16L * 1024 * 1024
$maximumWorkerHandles = 512
$maximumWorkerThreads = 16

$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
if ([System.IO.Path]::GetFileName($resolvedExecutable) -cne 'CursorPeek.exe') {
    throw "The performance gate requires CursorPeek.exe, not '$resolvedExecutable'."
}

$executableLength = (Get-Item -LiteralPath $resolvedExecutable).Length

function New-DiagnosticProcess {
    param(
        [Parameter(Mandatory)]
        [string] $Argument
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $resolvedExecutable
    $startInfo.Arguments = $Argument
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    return $process
}

function Stop-DiagnosticAfterTimeout {
    param(
        [Parameter(Mandatory)]
        [System.Diagnostics.Process] $Process,

        [Parameter(Mandatory)]
        [string] $Name
    )

    $Process.Kill()
    if (-not $Process.WaitForExit(5000)) {
        throw "$Name did not exit within five seconds after termination."
    }
    throw "$Name exceeded $TimeoutSeconds seconds."
}

function Get-ProcessSample {
    param(
        [Parameter(Mandatory)]
        [System.Diagnostics.Process] $Process
    )

    try {
        if ($Process.HasExited) {
            return $null
        }
        $Process.Refresh()
        $threads = @($Process.Threads)
        if ($threads.Count -eq 0 -or $Process.HasExited) {
            return $null
        }
        return [PSCustomObject]@{
            WorkingSet = [long] $Process.WorkingSet64
            PrivateBytes = [long] $Process.PrivateMemorySize64
            Handles = [int] $Process.HandleCount
            Threads = [int] $threads.Count
        }
    }
    catch [System.InvalidOperationException] {
        return $null
    }
    catch [System.ComponentModel.Win32Exception] {
        return $null
    }
}

function Get-WorkerSamples {
    param(
        [Parameter(Mandatory)]
        [int] $ParentProcessId
    )

    $samples = [System.Collections.Generic.List[object]]::new()
    $children = @(
        Get-CimInstance -ClassName Win32_Process -Filter "ParentProcessId = $ParentProcessId" `
            -ErrorAction SilentlyContinue
    )
    foreach ($child in $children) {
        if (
            [string]::IsNullOrWhiteSpace($child.ExecutablePath) -or
            [string]::IsNullOrWhiteSpace($child.CommandLine) -or
            $child.CommandLine -notmatch '(?i)(?:^|\s)--preview-worker(?:\s|$)'
        ) {
            continue
        }

        $childPath = [System.IO.Path]::GetFullPath($child.ExecutablePath)
        if (
            -not [string]::Equals(
                $childPath,
                $resolvedExecutable,
                [System.StringComparison]::OrdinalIgnoreCase
            )
        ) {
            continue
        }

        $childProcess = Get-Process -Id $child.ProcessId -ErrorAction SilentlyContinue
        if ($null -eq $childProcess) {
            continue
        }
        try {
            $sample = Get-ProcessSample $childProcess
            if ($null -ne $sample) {
                $samples.Add($sample)
            }
        }
        finally {
            $childProcess.Dispose()
        }
    }
    return @($samples)
}

function Get-ExactWorkers {
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

function Wait-ForNoWorkers {
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        $workers = @(Get-ExactWorkers)
        if ($workers.Count -eq 0) {
            return
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)

    $identifiers = ($workers | ForEach-Object ProcessId) -join ', '
    throw "The performance gate left worker processes behind: $identifiers."
}

function Get-NearestRankP95 {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IEnumerable] $Values
    )

    $ordered = @($Values | Sort-Object)
    if ($ordered.Count -eq 0) {
        throw 'Cannot calculate p95 without observations.'
    }
    $index = [int] [Math]::Ceiling($ordered.Count * 0.95) - 1
    return [long] $ordered[$index]
}

function Assert-AtMost {
    param(
        [Parameter(Mandatory)]
        [string] $Name,

        [Parameter(Mandatory)]
        [long] $Actual,

        [Parameter(Mandatory)]
        [long] $Limit,

        [Parameter(Mandatory)]
        [string] $Unit
    )

    if ($Actual -gt $Limit) {
        throw "$Name is $Actual $Unit; the release ceiling is $Limit $Unit."
    }
}

$preexistingWorkers = @(Get-ExactWorkers)
if ($preexistingWorkers.Count -ne 0) {
    $identifiers = ($preexistingWorkers | ForEach-Object ProcessId) -join ', '
    throw "Close the existing CursorPeek worker processes before the performance gate: $identifiers."
}

Assert-AtMost 'Release executable size' $executableLength $maximumExecutableBytes 'bytes'

$startupReadyMicroseconds = [System.Collections.Generic.List[long]]::new()
$workerElapsedMilliseconds = [System.Collections.Generic.List[long]]::new()
$coordinatorWorkingSet = 0L
$coordinatorPrivateBytes = 0L
$coordinatorHandles = 0
$coordinatorThreads = 0
$workerWorkingSet = 0L
$workerPrivateBytes = 0L
$workerHandles = 0
$workerThreads = 0
$coordinatorSampleCount = 0
$workerSampleCount = 0
$maximumIdleUiTask = 0L

$startupReport = '^Idle startup diagnostic completed: ready_us=([0-9]+), hold_ms=750, ' +
    'idle_ui_thread_max_us=([0-9]+), graceful_shutdown=yes$'

for ($run = 1; $run -le $Runs; $run++) {
    $process = New-DiagnosticProcess '--performance-diagnostics'
    $started = $false
    $wall = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        if (-not $process.Start()) {
            throw "Idle startup diagnostic run $run did not start."
        }
        $started = $true

        do {
            $sample = Get-ProcessSample $process
            if ($null -ne $sample) {
                $coordinatorWorkingSet = [Math]::Max(
                    $coordinatorWorkingSet,
                    $sample.WorkingSet
                )
                $coordinatorPrivateBytes = [Math]::Max(
                    $coordinatorPrivateBytes,
                    $sample.PrivateBytes
                )
                $coordinatorHandles = [Math]::Max($coordinatorHandles, $sample.Handles)
                $coordinatorThreads = [Math]::Max($coordinatorThreads, $sample.Threads)
                $coordinatorSampleCount++
            }
            if ($wall.Elapsed.TotalSeconds -gt $TimeoutSeconds) {
                Stop-DiagnosticAfterTimeout $process "Idle startup diagnostic run $run"
            }
        } while (-not $process.WaitForExit(20))

        $wall.Stop()
        $stdout = $process.StandardOutput.ReadToEnd().Trim()
        $stderr = $process.StandardError.ReadToEnd().Trim()
        if ($process.ExitCode -ne 0) {
            throw "Idle startup diagnostic run $run exited with code $($process.ExitCode): $stderr"
        }
        if (-not [string]::IsNullOrWhiteSpace($stderr)) {
            throw "Idle startup diagnostic run $run wrote unexpected stderr: $stderr"
        }
        if ($stdout -notmatch $startupReport) {
            throw "Idle startup diagnostic run $run returned an unexpected report: $stdout"
        }

        $reportedReadyMicroseconds = [long] $Matches[1]
        $coldStartUpperMicroseconds = [Math]::Max(
            0L,
            ($wall.ElapsedMilliseconds - 750L) * 1000L
        )
        $startupReadyMicroseconds.Add(
            [Math]::Max($reportedReadyMicroseconds, $coldStartUpperMicroseconds)
        )
        $maximumIdleUiTask = [Math]::Max($maximumIdleUiTask, [long] $Matches[2])
    }
    finally {
        $wall.Stop()
        if ($started -and -not $process.HasExited) {
            $process.Kill()
            if (-not $process.WaitForExit(5000)) {
                throw "Idle startup diagnostic run $run could not be terminated."
            }
        }
        $process.Dispose()
    }
}

$workerReport = '^Contained worker diagnostic completed: final_generation=4, ' +
    'status=Unavailable, requests=4, sessions=3, reuse=yes, idle_restart=yes, ' +
    'session_recycle=yes, elapsed=([0-9]+) ms$'

for ($run = 1; $run -le $Runs; $run++) {
    $process = New-DiagnosticProcess '--worker-diagnostics'
    $started = $false
    try {
        if (-not $process.Start()) {
            throw "Worker lifecycle diagnostic run $run did not start."
        }
        $started = $true
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            Stop-DiagnosticAfterTimeout $process "Worker lifecycle diagnostic run $run"
        }

        $stdout = $process.StandardOutput.ReadToEnd().Trim()
        $stderr = $process.StandardError.ReadToEnd().Trim()
        if ($process.ExitCode -ne 0) {
            throw "Worker lifecycle diagnostic run $run exited with code $($process.ExitCode): $stderr"
        }
        if (-not [string]::IsNullOrWhiteSpace($stderr)) {
            throw "Worker lifecycle diagnostic run $run wrote unexpected stderr: $stderr"
        }
        if ($stdout -notmatch $workerReport) {
            throw "Worker lifecycle diagnostic run $run returned an unexpected report: $stdout"
        }
        $workerElapsedMilliseconds.Add([long] $Matches[1])
    }
    finally {
        if ($started -and -not $process.HasExited) {
            $process.Kill()
            if (-not $process.WaitForExit(5000)) {
                throw "Worker lifecycle diagnostic run $run could not be terminated."
            }
        }
        $process.Dispose()
    }
    Wait-ForNoWorkers
}

$recoveryReport = '^Recovery soak completed: cycles=32, requests=132, sessions=69, ' +
    'taskbar_recoveries=32, power_cycles=32, idle_restarts=4, forced_timeouts=4, ' +
    'residual_workers=0, elapsed=([0-9]+) ms$'
$process = New-DiagnosticProcess '--recovery-soak-diagnostics'
$started = $false
$wall = [System.Diagnostics.Stopwatch]::StartNew()
try {
    if (-not $process.Start()) {
        throw 'Worker resource diagnostic did not start.'
    }
    $started = $true

    do {
        foreach ($sample in @(Get-WorkerSamples $process.Id)) {
            $workerWorkingSet = [Math]::Max($workerWorkingSet, $sample.WorkingSet)
            $workerPrivateBytes = [Math]::Max($workerPrivateBytes, $sample.PrivateBytes)
            $workerHandles = [Math]::Max($workerHandles, $sample.Handles)
            $workerThreads = [Math]::Max($workerThreads, $sample.Threads)
            $workerSampleCount++
        }
        if ($wall.Elapsed.TotalSeconds -gt $TimeoutSeconds) {
            Stop-DiagnosticAfterTimeout $process 'Worker resource diagnostic'
        }
    } while (-not $process.WaitForExit(20))

    $stdout = $process.StandardOutput.ReadToEnd().Trim()
    $stderr = $process.StandardError.ReadToEnd().Trim()
    if ($process.ExitCode -ne 0) {
        throw "Worker resource diagnostic exited with code $($process.ExitCode): $stderr"
    }
    if (-not [string]::IsNullOrWhiteSpace($stderr)) {
        throw "Worker resource diagnostic wrote unexpected stderr: $stderr"
    }
    if ($stdout -notmatch $recoveryReport) {
        throw "Worker resource diagnostic returned an unexpected report: $stdout"
    }
}
finally {
    $wall.Stop()
    if ($started -and -not $process.HasExited) {
        $process.Kill()
        if (-not $process.WaitForExit(5000)) {
            throw 'Worker resource diagnostic could not be terminated.'
        }
    }
    $process.Dispose()
}
Wait-ForNoWorkers

if ($coordinatorSampleCount -eq 0) {
    throw 'The performance gate did not observe the idle coordinator process.'
}
if ($workerSampleCount -eq 0) {
    throw 'The performance gate did not observe a contained worker process.'
}

$startupP95 = Get-NearestRankP95 $startupReadyMicroseconds
$workerP95 = Get-NearestRankP95 $workerElapsedMilliseconds

Assert-AtMost 'Idle startup ready p95' $startupP95 $maximumStartupP95Microseconds 'microseconds'
Assert-AtMost 'Worker lifecycle p95' $workerP95 $maximumWorkerP95Milliseconds 'milliseconds'
Assert-AtMost 'Idle UI-thread task maximum' $maximumIdleUiTask `
    $maximumIdleUiTaskMicroseconds 'microseconds'
Assert-AtMost 'Coordinator working set' $coordinatorWorkingSet `
    $maximumCoordinatorWorkingSetBytes 'bytes'
Assert-AtMost 'Coordinator private bytes' $coordinatorPrivateBytes `
    $maximumCoordinatorPrivateBytes 'bytes'
Assert-AtMost 'Coordinator handle count' $coordinatorHandles $maximumCoordinatorHandles 'handles'
Assert-AtMost 'Coordinator thread count' $coordinatorThreads $maximumCoordinatorThreads 'threads'
Assert-AtMost 'Worker working set' $workerWorkingSet $maximumWorkerWorkingSetBytes 'bytes'
Assert-AtMost 'Worker private bytes' $workerPrivateBytes $maximumWorkerPrivateBytes 'bytes'
Assert-AtMost 'Worker handle count' $workerHandles $maximumWorkerHandles 'handles'
Assert-AtMost 'Worker thread count' $workerThreads $maximumWorkerThreads 'threads'

(
    "Performance budget gate passed: runs={0}, exe_bytes={1}, startup_p95_us={2}, " +
    "worker_p95_ms={3}, idle_ui_task_max_us={4}, coordinator_working_set={5}, " +
    "coordinator_private_bytes={6}, coordinator_handles={7}, coordinator_threads={8}, " +
    "worker_working_set={9}, worker_private_bytes={10}, worker_handles={11}, " +
    "worker_threads={12}, coordinator_samples={13}, worker_samples={14}"
) -f @(
    $Runs,
    $executableLength,
    $startupP95,
    $workerP95,
    $maximumIdleUiTask,
    $coordinatorWorkingSet,
    $coordinatorPrivateBytes,
    $coordinatorHandles,
    $coordinatorThreads,
    $workerWorkingSet,
    $workerPrivateBytes,
    $workerHandles,
    $workerThreads,
    $coordinatorSampleCount,
    $workerSampleCount
)
