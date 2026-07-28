[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $InstallerPath,

    [Parameter()]
    [switch] $AllowDirtyMetadata,

    [Parameter()]
    [switch] $ExerciseInstall,

    [Parameter()]
    [switch] $KeepTestFiles
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$resolvedInstaller = (Resolve-Path -LiteralPath $InstallerPath).Path
$sidecarPath = "$resolvedInstaller.sha256"
$utf8Strict = [System.Text.UTF8Encoding]::new($false, $true)
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$testRoot = ''
$installRoot = ''
$ownsCurrentUserState = $false

function Get-Sha256Hex {
    param([Parameter(Mandatory = $true)][string] $LiteralPath)

    $stream = [System.IO.File]::OpenRead($LiteralPath)
    try {
        $algorithm = [System.Security.Cryptography.SHA256]::Create()
        try {
            return [System.BitConverter]::ToString(
                $algorithm.ComputeHash($stream)
            ).Replace('-', '')
        }
        finally {
            $algorithm.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Read-StrictUtf8 {
    param([Parameter(Mandatory = $true)][string] $LiteralPath)

    $bytes = [System.IO.File]::ReadAllBytes($LiteralPath)
    $text = $utf8Strict.GetString($bytes)
    if ($text.Length -gt 0 -and $text[0] -eq [char] 0xFEFF) {
        throw "Text file unexpectedly contains a UTF-8 BOM: '$LiteralPath'."
    }
    if ($text.Contains("`r")) {
        throw "Text file does not use canonical LF endings: '$LiteralPath'."
    }
    return $text
}

function Get-RelativePath {
    param(
        [Parameter(Mandatory = $true)][string] $Root,
        [Parameter(Mandatory = $true)][string] $Path
    )

    $rootPrefix = [System.IO.Path]::GetFullPath($Root).TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    ) + [System.IO.Path]::DirectorySeparatorChar
    $resolvedPath = [System.IO.Path]::GetFullPath($Path)
    if (-not $resolvedPath.StartsWith(
        $rootPrefix,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Installed file escapes its expected root: '$resolvedPath'."
    }
    return $resolvedPath.Substring($rootPrefix.Length).Replace('\', '/')
}

function Invoke-NativeProcess {
    param(
        [Parameter(Mandatory = $true)][string] $FileName,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string] $Arguments,
        [Parameter()][ValidateRange(1, 300)][int] $TimeoutSeconds = 120
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FileName
    $startInfo.Arguments = $Arguments
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw "Could not start '$FileName'."
        }
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill()
            $process.WaitForExit()
            throw "Process '$FileName' timed out."
        }
        if ($process.ExitCode -ne 0) {
            throw "Process '$FileName' exited with code $($process.ExitCode)."
        }
    }
    finally {
        $process.Dispose()
    }
}

function Invoke-CursorPeek {
    param(
        [Parameter(Mandatory = $true)][string] $Executable,
        [Parameter(Mandatory = $true)][string] $Argument,
        [Parameter(Mandatory = $true)][string] $WorkingDirectory,
        [Parameter()][ValidateRange(1, 120)][int] $TimeoutSeconds = 30
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.Arguments = $Argument
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw "Could not run installed CursorPeek with '$Argument'."
        }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill()
            $process.WaitForExit()
            throw "Installed CursorPeek timed out with '$Argument'."
        }
        $stdout = $stdoutTask.Result
        $stderr = $stderrTask.Result
        if ($process.ExitCode -ne 0) {
            throw (
                "Installed CursorPeek exited with code $($process.ExitCode) for " +
                "'$Argument': $($stderr.Trim())"
            )
        }
        if (-not [string]::IsNullOrWhiteSpace($stderr)) {
            throw "Installed CursorPeek wrote unexpected stderr: '$($stderr.Trim())'."
        }
        return $stdout.Trim()
    }
    finally {
        $process.Dispose()
    }
}

function Wait-Until {
    param(
        [Parameter(Mandatory = $true)][scriptblock] $Condition,
        [Parameter(Mandatory = $true)][string] $Failure,
        [Parameter()][ValidateRange(1, 60)][int] $TimeoutSeconds = 15
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) {
            return
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw $Failure
}

if (-not [System.IO.File]::Exists($sidecarPath)) {
    throw "Installer checksum sidecar is missing: '$sidecarPath'."
}
$installerName = [System.IO.Path]::GetFileName($resolvedInstaller)
$nameMatch = [Regex]::Match(
    $installerName,
    '^CursorPeek-([A-Za-z0-9][A-Za-z0-9.+-]*)-windows-x64-setup\.exe$',
    [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
)
if (-not $nameMatch.Success) {
    throw "Installer filename is invalid: '$installerName'."
}
$version = $nameMatch.Groups[1].Value

$sidecarText = Read-StrictUtf8 $sidecarPath
$sidecarMatch = [Regex]::Match(
    $sidecarText,
    '^([0-9A-F]{64})  ([^/\r\n]+)\n$',
    [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
)
if (-not $sidecarMatch.Success -or $sidecarMatch.Groups[2].Value -cne $installerName) {
    throw 'Installer checksum sidecar has an invalid canonical record.'
}
$installerHash = Get-Sha256Hex $resolvedInstaller
if ($installerHash -cne $sidecarMatch.Groups[1].Value) {
    throw 'Installer does not match its SHA-256 sidecar.'
}

$installerItem = Get-Item -LiteralPath $resolvedInstaller
if ($installerItem.Length -lt 100000 -or $installerItem.Length -gt 104857600) {
    throw "Installer size $($installerItem.Length) bytes is outside the expected bounds."
}
$versionInfo = $installerItem.VersionInfo
if ($versionInfo.ProductName -cne 'CursorPeek' -or
    $versionInfo.ProductVersion -cne $version -or
    $versionInfo.FileDescription -cne 'CursorPeek per-user installer' -or
    $versionInfo.OriginalFilename -cne $installerName) {
    throw 'Installer Windows version resources are incomplete or inconsistent.'
}
$signature = Get-AuthenticodeSignature -LiteralPath $resolvedInstaller
if ($signature.Status -notin @(
    [System.Management.Automation.SignatureStatus]::NotSigned,
    [System.Management.Automation.SignatureStatus]::Valid
)) {
    throw "Installer Authenticode status is '$($signature.Status)'."
}

if (-not $ExerciseInstall) {
    Write-Output (
        "Installer structure passed: version=$version, bytes=$($installerItem.Length), " +
        "signature=$($signature.Status), sha256=$installerHash"
    )
    return
}

if (-not [Environment]::Is64BitOperatingSystem -or
    -not [Environment]::Is64BitProcess) {
    throw 'Installer lifecycle testing requires a 64-bit PowerShell process on 64-bit Windows.'
}

$localAppData = [Environment]::GetFolderPath(
    [Environment+SpecialFolder]::LocalApplicationData
)
$startMenu = [Environment]::GetFolderPath(
    [Environment+SpecialFolder]::StartMenu
)
$desktop = [Environment]::GetFolderPath(
    [Environment+SpecialFolder]::DesktopDirectory
)
if ([string]::IsNullOrWhiteSpace($localAppData) -or
    [string]::IsNullOrWhiteSpace($startMenu) -or
    [string]::IsNullOrWhiteSpace($desktop)) {
    throw 'Windows did not resolve the current-user shell folders.'
}

$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\CursorPeek'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$configDirectory = Join-Path $localAppData 'CursorPeek'
$configPath = Join-Path $configDirectory 'config.ini'
$startMenuDirectory = Join-Path $startMenu 'Programs\CursorPeek'
$startMenuShortcut = Join-Path $startMenuDirectory 'CursorPeek.lnk'
$startMenuUninstall = Join-Path $startMenuDirectory 'Uninstall CursorPeek.lnk'
$desktopShortcut = Join-Path $desktop 'CursorPeek.lnk'

$existingRunValue = $null
try {
    $existingRunValue = Get-ItemPropertyValue `
        -LiteralPath $runKey `
        -Name 'CursorPeek' `
        -ErrorAction Stop
}
catch [System.Management.Automation.ItemNotFoundException] {
}
catch [System.Management.Automation.PSArgumentException] {
}

$preexisting = @(
    @(
        [PSCustomObject] @{
            Label = 'uninstall registration'
            Exists = Test-Path $uninstallKey
        },
        [PSCustomObject] @{
            Label = 'installed configuration'
            Exists = Test-Path $configDirectory
        },
        [PSCustomObject] @{
            Label = 'Start Menu shortcuts'
            Exists = Test-Path $startMenuDirectory
        },
        [PSCustomObject] @{
            Label = 'desktop shortcut'
            Exists = Test-Path $desktopShortcut
        },
        [PSCustomObject] @{
            Label = 'startup registration'
            Exists = -not [string]::IsNullOrEmpty([string] $existingRunValue)
        }
    ) | Where-Object { $_.Exists }
)
if ($preexisting.Count -ne 0) {
    throw (
        'Installer lifecycle test requires a clean current-user CursorPeek state; found ' +
        (($preexisting | ForEach-Object { $_.Label }) -join ', ') + '.'
    )
}
if (@(Get-Process -Name CursorPeek -ErrorAction SilentlyContinue).Count -ne 0) {
    throw 'Installer lifecycle test requires no running CursorPeek process.'
}

$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    'cursorpeek-installer-test-' + [System.Guid]::NewGuid().ToString('N')
)
$installRoot = Join-Path $testRoot 'Installed CursorPeek'
[System.IO.Directory]::CreateDirectory($testRoot) | Out-Null
$ownsCurrentUserState = $true

try {
    Invoke-NativeProcess `
        $resolvedInstaller `
        "/S /STARTMENU=1 /DESKTOP=0 /STARTUP=0 /D=$installRoot"

    $executable = Join-Path $installRoot 'CursorPeek.exe'
    $uninstaller = Join-Path $installRoot 'Uninstall.exe'
    foreach ($required in @(
        $executable,
        $uninstaller,
        (Join-Path $installRoot 'CHANGELOG.md'),
        (Join-Path $installRoot 'README.txt'),
        (Join-Path $installRoot 'RELEASE-METADATA.json'),
        (Join-Path $installRoot 'SHA256SUMS.txt'),
        (Join-Path $installRoot 'THIRD-PARTY-NOTICES.txt'),
        (Join-Path $installRoot 'docs\KNOWN_LIMITATIONS.md'),
        (Join-Path $installRoot 'docs\USER_GUIDE.md'),
        (Join-Path $installRoot 'licenses\LICENSE-MIT'),
        (Join-Path $installRoot 'licenses\LICENSE-APACHE'),
        (Join-Path $installRoot 'licenses\packaging\NSIS-COPYING')
    )) {
        if (-not [System.IO.File]::Exists($required)) {
            throw "Default installation is missing '$required'."
        }
    }
    if ([System.IO.File]::Exists((Join-Path $installRoot 'CursorPeek.portable'))) {
        throw 'Installed package must not contain the portable marker.'
    }

    $metadata = (
        Read-StrictUtf8 (Join-Path $installRoot 'RELEASE-METADATA.json')
    ) | ConvertFrom-Json
    if ([int] $metadata.schema_version -ne 1 -or
        [string] $metadata.product -cne 'CursorPeek' -or
        [string] $metadata.version -cne $version -or
        [string] $metadata.package_kind -cne 'installer' -or
        [string] $metadata.architecture -cne 'x64' -or
        [string] $metadata.packager.name -cne 'NSIS' -or
        [string] $metadata.packager.version -cne '3.12' -or
        [string] $metadata.packager.license_file -cne
            'licenses/packaging/NSIS-COPYING') {
        throw 'Installed release metadata does not identify the expected package.'
    }
    if ([bool] $metadata.source_dirty -and -not $AllowDirtyMetadata) {
        throw 'Installed metadata identifies a dirty source tree.'
    }
    $notices = Read-StrictUtf8 (Join-Path $installRoot 'THIRD-PARTY-NOTICES.txt')
    if (-not $notices.Contains("Packaging technology: NSIS 3.12`n") -or
        -not $notices.Contains("License file: licenses/packaging/NSIS-COPYING`n")) {
        throw 'Installed third-party notices omit the NSIS packaging technology.'
    }
    $executableItem = Get-Item -LiteralPath $executable
    if ([long] $metadata.executable.bytes -ne $executableItem.Length -or
        [string] $metadata.executable.sha256 -cne (Get-Sha256Hex $executable)) {
        throw 'Installed release metadata does not match CursorPeek.exe.'
    }

    $checksumPath = Join-Path $installRoot 'SHA256SUMS.txt'
    $readmePath = Join-Path $installRoot 'README.txt'
    $packagedReadmeHash = Get-Sha256Hex $readmePath
    $checksumPaths = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($line in @(
        (Read-StrictUtf8 $checksumPath).Split("`n") |
            Where-Object { $_.Length -gt 0 }
    )) {
        $match = [Regex]::Match(
            $line,
            '^([0-9A-F]{64})  ([^\\:\r\n]+)$',
            [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
        )
        if (-not $match.Success -or -not $checksumPaths.Add($match.Groups[2].Value)) {
            throw "Installed checksum manifest has an invalid record '$line'."
        }
        $checkedFile = Join-Path $installRoot ($match.Groups[2].Value.Replace('/', '\'))
        if (-not [System.IO.File]::Exists($checkedFile) -or
            (Get-Sha256Hex $checkedFile) -cne $match.Groups[1].Value) {
            throw "Installed checksum does not match '$($match.Groups[2].Value)'."
        }
    }
    $installedPayloadFiles = @(
        Get-ChildItem -LiteralPath $installRoot -Recurse -File |
            Where-Object {
                $_.FullName -cne $checksumPath -and
                $_.FullName -cne $uninstaller
            }
    )
    if ($checksumPaths.Count -ne $installedPayloadFiles.Count) {
        throw 'Installed checksums do not cover every packaged payload file exactly once.'
    }
    foreach ($file in $installedPayloadFiles) {
        if (-not $checksumPaths.Contains((Get-RelativePath $installRoot $file.FullName))) {
            throw "Installed checksums omit '$($file.FullName)'."
        }
    }

    $versionOutput = Invoke-CursorPeek $executable '--version' $installRoot
    if ($versionOutput -cne "CursorPeek $version") {
        throw "Installed executable returned an unexpected version '$versionOutput'."
    }
    $settingsOutput = Invoke-CursorPeek `
        $executable `
        '--settings-diagnostics' `
        $installRoot
    if ($settingsOutput -cne (
        'Settings storage diagnostic completed: mode=installed, configuration_created=yes'
    )) {
        throw "Installed storage diagnostic returned '$settingsOutput'."
    }
    $configText = Read-StrictUtf8 $configPath
    if (-not $configText.Contains("cache_entries=128`n") -or
        -not $configText.Contains("start_with_windows=false`n")) {
        throw 'Default installation did not persist canonical performance and startup settings.'
    }
    if (-not (Test-Path -LiteralPath $startMenuShortcut) -or
        -not (Test-Path -LiteralPath $startMenuUninstall) -or
        (Test-Path -LiteralPath $desktopShortcut)) {
        throw 'Default shortcut selection was not applied.'
    }
    $defaultRunValue = $null
    try {
        $defaultRunValue = Get-ItemPropertyValue `
            -LiteralPath $runKey `
            -Name 'CursorPeek' `
            -ErrorAction Stop
    }
    catch [System.Management.Automation.ItemNotFoundException] {
    }
    catch [System.Management.Automation.PSArgumentException] {
    }
    if (-not [string]::IsNullOrEmpty([string] $defaultRunValue)) {
        throw 'Default installation unexpectedly enabled startup.'
    }

    $registration = Get-ItemProperty -LiteralPath $uninstallKey
    if ($registration.DisplayName -cne 'CursorPeek' -or
        $registration.DisplayVersion -cne $version -or
        $registration.InstallLocation -cne $installRoot -or
        [int] $registration.StartMenuShortcut -ne 1 -or
        [int] $registration.DesktopShortcut -ne 0 -or
        [int] $registration.StartWithWindows -ne 0) {
        throw 'Default uninstall registration is incomplete.'
    }

    $sentinelPath = Join-Path $installRoot 'user-owned-sentinel.txt'
    [System.IO.File]::WriteAllText(
        $sentinelPath,
        "preserve user file`n",
        $utf8WithoutBom
    )
    $preservedConfig = $configText.Replace(
        'dwell_delay_ms=50',
        'dwell_delay_ms=700'
    ) + "future_installer_test=preserved`n"
    [System.IO.File]::WriteAllText($configPath, $preservedConfig, $utf8WithoutBom)

    $predecessorVersion = '0.0.0-test'
    Set-ItemProperty `
        -LiteralPath $uninstallKey `
        -Name 'DisplayVersion' `
        -Value $predecessorVersion
    [System.IO.File]::WriteAllText(
        $readmePath,
        "CursorPeek predecessor payload fixture`n",
        $utf8WithoutBom
    )
    if ((Get-Sha256Hex $readmePath) -ceq $packagedReadmeHash) {
        throw 'Predecessor fixture did not replace the installed payload.'
    }

    $running = Start-Process -FilePath $executable -PassThru
    Start-Sleep -Milliseconds 750
    if ($running.HasExited) {
        throw 'CursorPeek exited before upgrade could exercise graceful shutdown.'
    }

    Invoke-NativeProcess `
        $resolvedInstaller `
        "/S /STARTMENU=1 /DESKTOP=1 /STARTUP=1 /D=$installRoot"
    Wait-Until `
        { $running.HasExited } `
        'Upgrade did not gracefully stop the running CursorPeek instance.'
    $running.Dispose()

    if (-not [System.IO.File]::Exists($sentinelPath) -or
        (Read-StrictUtf8 $configPath) -notmatch '(?m)^dwell_delay_ms=700$' -or
        (Read-StrictUtf8 $configPath) -notmatch '(?m)^future_installer_test=preserved$' -or
        (Read-StrictUtf8 $configPath) -notmatch '(?m)^start_with_windows=true$') {
        throw 'Upgrade did not preserve user files and settings while applying startup.'
    }
    if ((Get-Sha256Hex $readmePath) -cne $packagedReadmeHash) {
        throw 'Upgrade did not replace the predecessor payload.'
    }
    if (-not (Test-Path -LiteralPath $desktopShortcut) -or
        -not (Test-Path -LiteralPath $startMenuShortcut)) {
        throw 'Upgrade did not apply the requested shortcut selections.'
    }
    $expectedRunCommand = '"' + $executable + '"'
    $runValue = Get-ItemPropertyValue -LiteralPath $runKey -Name 'CursorPeek'
    if ([string] $runValue -cne $expectedRunCommand) {
        throw "Upgrade wrote an unexpected startup command '$runValue'."
    }
    $registration = Get-ItemProperty -LiteralPath $uninstallKey
    if ($registration.DisplayVersion -cne $version -or
        [int] $registration.StartMenuShortcut -ne 1 -or
        [int] $registration.DesktopShortcut -ne 1 -or
        [int] $registration.StartWithWindows -ne 1) {
        throw 'Upgrade did not restore the current version and component selections.'
    }

    Invoke-NativeProcess $uninstaller '/S'
    Wait-Until `
        { -not (Test-Path -LiteralPath $uninstallKey) } `
        'Silent uninstall did not remove its registration.'

    foreach ($removed in @(
        $executable,
        $uninstaller,
        $startMenuShortcut,
        $startMenuUninstall,
        $desktopShortcut,
        $configPath
    )) {
        if (Test-Path -LiteralPath $removed) {
            throw "Uninstall left owned state '$removed'."
        }
    }
    $remainingRunValue = $null
    try {
        $remainingRunValue = Get-ItemPropertyValue `
            -LiteralPath $runKey `
            -Name 'CursorPeek' `
            -ErrorAction Stop
    }
    catch [System.Management.Automation.ItemNotFoundException] {
    }
    catch [System.Management.Automation.PSArgumentException] {
    }
    if (-not [string]::IsNullOrEmpty([string] $remainingRunValue)) {
        throw 'Uninstall left the CursorPeek startup value.'
    }
    if (-not [System.IO.File]::Exists($sentinelPath)) {
        throw 'Uninstall removed a user-owned file from the installation directory.'
    }
    $remainingFiles = @(
        Get-ChildItem -LiteralPath $installRoot -Recurse -File
    )
    if ($remainingFiles.Count -ne 1 -or
        $remainingFiles[0].FullName -cne $sentinelPath) {
        throw 'Uninstall left files other than the user-owned sentinel.'
    }
    if (@(Get-Process -Name CursorPeek -ErrorAction SilentlyContinue).Count -ne 0) {
        throw 'Installer lifecycle test left CursorPeek running.'
    }

    [System.IO.File]::Delete($sentinelPath)
    [System.IO.Directory]::Delete($installRoot)

    Invoke-NativeProcess `
        $resolvedInstaller `
        "/S /STARTMENU=0 /DESKTOP=0 /STARTUP=0 /D=$installRoot"
    $uninstaller = Join-Path $installRoot 'Uninstall.exe'
    Invoke-NativeProcess $uninstaller '/S'
    Wait-Until `
        {
            -not (Test-Path -LiteralPath $installRoot) -and
            -not (Test-Path -LiteralPath $uninstallKey)
        } `
        'Clean uninstall left its installation directory or registration.'
    foreach ($residue in @(
        $configDirectory,
        $startMenuDirectory,
        $desktopShortcut
    )) {
        if (Test-Path -LiteralPath $residue) {
            throw "Clean uninstall left product state '$residue'."
        }
    }
    $remainingRunValue = $null
    try {
        $remainingRunValue = Get-ItemPropertyValue `
            -LiteralPath $runKey `
            -Name 'CursorPeek' `
            -ErrorAction Stop
    }
    catch [System.Management.Automation.ItemNotFoundException] {
    }
    catch [System.Management.Automation.PSArgumentException] {
    }
    if (-not [string]::IsNullOrEmpty([string] $remainingRunValue)) {
        throw 'Clean uninstall left the CursorPeek startup value.'
    }
    if (@(Get-Process -Name CursorPeek -ErrorAction SilentlyContinue).Count -ne 0) {
        throw 'Clean uninstall left CursorPeek running.'
    }

    Write-Output (
        (
            "Installer lifecycle passed: version={0}, default_install=yes, upgrade=yes, " +
            "settings_preserved=yes, running_app_shutdown=yes, uninstall=yes, " +
            "user_file_preserved=yes, zero_residue=yes, sha256={1}"
        ) -f
        $version,
        $installerHash
    )
}
finally {
    if ($ownsCurrentUserState) {
        $cleanupUninstaller = if ([string]::IsNullOrWhiteSpace($installRoot)) {
            ''
        }
        else {
            Join-Path $installRoot 'Uninstall.exe'
        }
        if (-not [string]::IsNullOrWhiteSpace($cleanupUninstaller) -and
            [System.IO.File]::Exists($cleanupUninstaller)) {
            try {
                Invoke-NativeProcess $cleanupUninstaller '/S' -TimeoutSeconds 60
            }
            catch {
                Write-Warning "Best-effort uninstall cleanup failed: $_"
            }
        }

        Remove-Item -LiteralPath $startMenuShortcut -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $startMenuUninstall -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $startMenuDirectory -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $desktopShortcut -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $uninstallKey -Recurse -Force -ErrorAction SilentlyContinue
        $cleanupRunValue = $null
        try {
            $cleanupRunValue = Get-ItemPropertyValue `
                -LiteralPath $runKey `
                -Name 'CursorPeek' `
                -ErrorAction Stop
        }
        catch {
        }
        if (-not [string]::IsNullOrWhiteSpace($installRoot) -and
            [string] $cleanupRunValue -ceq ('"' + (Join-Path $installRoot 'CursorPeek.exe') + '"')) {
            Remove-ItemProperty `
                -LiteralPath $runKey `
                -Name 'CursorPeek' `
                -Force `
                -ErrorAction SilentlyContinue
        }
        if ([System.IO.Directory]::Exists($configDirectory)) {
            [System.IO.Directory]::Delete($configDirectory, $true)
        }

        if (-not [string]::IsNullOrWhiteSpace($installRoot)) {
            $expectedExecutable = [System.IO.Path]::GetFullPath(
                (Join-Path $installRoot 'CursorPeek.exe')
            )
            foreach ($process in @(
                Get-CimInstance `
                    Win32_Process `
                    -Filter "Name = 'CursorPeek.exe'" `
                    -ErrorAction SilentlyContinue
            )) {
                if (-not [string]::IsNullOrWhiteSpace($process.ExecutablePath) -and
                    [string]::Equals(
                        [System.IO.Path]::GetFullPath($process.ExecutablePath),
                        $expectedExecutable,
                        [System.StringComparison]::OrdinalIgnoreCase
                    )) {
                    Invoke-CimMethod `
                        -InputObject $process `
                        -MethodName Terminate `
                        -ErrorAction SilentlyContinue | Out-Null
                }
            }
        }
    }

    if (-not $KeepTestFiles -and
        -not [string]::IsNullOrWhiteSpace($testRoot) -and
        [System.IO.Directory]::Exists($testRoot)) {
        [System.IO.Directory]::Delete($testRoot, $true)
    }
}
