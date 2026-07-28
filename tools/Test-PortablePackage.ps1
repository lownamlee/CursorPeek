[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $PackagePath,

    [Parameter()]
    [switch] $AllowDirtyMetadata,

    [Parameter()]
    [switch] $KeepExtracted
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.IO.Compression

$resolvedPackage = (Resolve-Path -LiteralPath $PackagePath).Path
$sidecarPath = "$resolvedPackage.sha256"
$utf8Strict = [System.Text.UTF8Encoding]::new($false, $true)
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$fixedTimestamp = [System.DateTime]::new(1980, 1, 1, 0, 0, 0)
$testRoot = ''

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

function Assert-SafeArchivePath {
    param([Parameter(Mandatory = $true)][string] $Path)

    if ([string]::IsNullOrWhiteSpace($Path) -or
        $Path.Contains('\') -or
        $Path.StartsWith('/') -or
        $Path.Contains(':')) {
        throw "Archive contains an unsafe path '$Path'."
    }
    $segments = @($Path.Split('/'))
    if ($segments.Count -lt 2) {
        throw "Archive entry is not beneath one package root: '$Path'."
    }
    foreach ($segment in $segments) {
        if ([string]::IsNullOrWhiteSpace($segment) -or
            $segment -ceq '.' -or
            $segment -ceq '..') {
            throw "Archive contains an unsafe path segment in '$Path'."
        }
    }
}

function Assert-ChildPath {
    param(
        [Parameter(Mandatory = $true)][string] $Root,
        [Parameter(Mandatory = $true)][string] $Candidate
    )

    $rootPrefix = [System.IO.Path]::GetFullPath($Root).TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    ) + [System.IO.Path]::DirectorySeparatorChar
    $resolvedCandidate = [System.IO.Path]::GetFullPath($Candidate)
    if (-not $resolvedCandidate.StartsWith(
        $rootPrefix,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Extracted path escapes its temporary root: '$resolvedCandidate'."
    }
    return $resolvedCandidate
}

function Get-RelativePackagePath {
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
        throw "Package file escapes its root: '$resolvedPath'."
    }
    return $resolvedPath.Substring($rootPrefix.Length).Replace('\', '/')
}

function Get-OptionalFileSnapshot {
    param([Parameter(Mandatory = $true)][string] $LiteralPath)

    if (-not [System.IO.File]::Exists($LiteralPath)) {
        return [PSCustomObject] @{
            Exists = $false
            Length = [long] 0
            Hash = ''
        }
    }
    $item = Get-Item -LiteralPath $LiteralPath
    return [PSCustomObject] @{
        Exists = $true
        Length = [long] $item.Length
        Hash = Get-Sha256Hex $item.FullName
    }
}

function Assert-SameSnapshot {
    param(
        [Parameter(Mandatory = $true)][object] $Before,
        [Parameter(Mandatory = $true)][object] $After,
        [Parameter(Mandatory = $true)][string] $Label
    )

    if ($Before.Exists -ne $After.Exists -or
        $Before.Length -ne $After.Length -or
        [string] $Before.Hash -cne [string] $After.Hash) {
        throw "$Label changed during the portable smoke test."
    }
}

function Get-OptionalRegistryValueSnapshot {
    param(
        [Parameter(Mandatory = $true)][string] $LiteralPath,
        [Parameter(Mandatory = $true)][string] $Name
    )

    try {
        $value = Get-ItemPropertyValue `
            -LiteralPath $LiteralPath `
            -Name $Name `
            -ErrorAction Stop
        return [PSCustomObject] @{
            Exists = $true
            Value = [string] $value
        }
    }
    catch [System.Management.Automation.ItemNotFoundException] {
        return [PSCustomObject] @{
            Exists = $false
            Value = ''
        }
    }
    catch [System.Management.Automation.PSArgumentException] {
        return [PSCustomObject] @{
            Exists = $false
            Value = ''
        }
    }
}

function Assert-SameRegistrySnapshot {
    param(
        [Parameter(Mandatory = $true)][object] $Before,
        [Parameter(Mandatory = $true)][object] $After,
        [Parameter(Mandatory = $true)][string] $Label
    )

    if ($Before.Exists -ne $After.Exists -or
        [string] $Before.Value -cne [string] $After.Value) {
        throw "$Label changed during the portable smoke test."
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
            throw "Could not start relocated CursorPeek with '$Argument'."
        }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill()
            $process.WaitForExit()
            throw "Relocated CursorPeek timed out with '$Argument'."
        }
        $stdout = $stdoutTask.Result
        $stderr = $stderrTask.Result
        if ($process.ExitCode -ne 0) {
            throw (
                "Relocated CursorPeek exited with code $($process.ExitCode) for '$Argument': " +
                $stderr.Trim()
            )
        }
        if (-not [string]::IsNullOrWhiteSpace($stderr)) {
            throw "Relocated CursorPeek wrote unexpected stderr for '$Argument': $($stderr.Trim())"
        }
        return $stdout.Trim()
    }
    finally {
        $process.Dispose()
    }
}

if (-not [System.IO.File]::Exists($sidecarPath)) {
    throw "Portable checksum sidecar is missing: '$sidecarPath'."
}
$archiveName = [System.IO.Path]::GetFileName($resolvedPackage)
$nameMatch = [Regex]::Match(
    $archiveName,
    '^CursorPeek-([A-Za-z0-9][A-Za-z0-9.+-]*)-windows-x64-portable\.zip$',
    [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
)
if (-not $nameMatch.Success) {
    throw "Portable archive name is invalid: '$archiveName'."
}
$version = $nameMatch.Groups[1].Value
$packageRootName = "CursorPeek-$version"

$sidecarText = Read-StrictUtf8 $sidecarPath
$sidecarMatch = [Regex]::Match(
    $sidecarText,
    '^([0-9A-F]{64})  ([^/\r\n]+)\n$',
    [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
)
if (-not $sidecarMatch.Success -or $sidecarMatch.Groups[2].Value -cne $archiveName) {
    throw 'Portable checksum sidecar has an invalid canonical record.'
}
$archiveHash = Get-Sha256Hex $resolvedPackage
if ($archiveHash -cne $sidecarMatch.Groups[1].Value) {
    throw 'Portable archive does not match its SHA-256 sidecar.'
}

$entryNames = [System.Collections.Generic.List[string]]::new()
$entryNameSet = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
$archiveStream = [System.IO.File]::OpenRead($resolvedPackage)
try {
    $archive = [System.IO.Compression.ZipArchive]::new(
        $archiveStream,
        [System.IO.Compression.ZipArchiveMode]::Read,
        $false
    )
    try {
        if ($archive.Entries.Count -eq 0) {
            throw 'Portable archive is empty.'
        }
        foreach ($entry in $archive.Entries) {
            $name = [string] $entry.FullName
            Assert-SafeArchivePath $name
            if (-not $entryNameSet.Add($name)) {
                throw "Portable archive repeats entry '$name'."
            }
            if ($entry.LastWriteTime.DateTime -ne $fixedTimestamp) {
                throw "Portable archive entry has a variable timestamp: '$name'."
            }
            if ($entry.CompressedLength -lt $entry.Length) {
                throw "Portable archive entry unexpectedly uses size-reducing compression: '$name'."
            }
            $entryNames.Add($name)
        }
    }
    finally {
        $archive.Dispose()
    }
}
finally {
    $archiveStream.Dispose()
}

$sortedEntryNames = [string[]] $entryNames.ToArray().Clone()
[System.Array]::Sort($sortedEntryNames, [System.StringComparer]::Ordinal)
for ($index = 0; $index -lt $sortedEntryNames.Count; $index++) {
    if ($sortedEntryNames[$index] -cne $entryNames[$index]) {
        throw 'Portable archive entries are not in ordinal path order.'
    }
}

$requiredEntries = @(
    "$packageRootName/CursorPeek.exe",
    "$packageRootName/CursorPeek.portable",
    "$packageRootName/CHANGELOG.md",
    "$packageRootName/README.txt",
    "$packageRootName/RELEASE-METADATA.json",
    "$packageRootName/SHA256SUMS.txt",
    "$packageRootName/THIRD-PARTY-NOTICES.txt",
    "$packageRootName/docs/KNOWN_LIMITATIONS.md",
    "$packageRootName/docs/PRIVACY.md",
    "$packageRootName/docs/SECURITY.md",
    "$packageRootName/docs/THREAT_MODEL.md",
    "$packageRootName/docs/USER_GUIDE.md",
    "$packageRootName/licenses/LICENSE-APACHE",
    "$packageRootName/licenses/LICENSE-MIT"
)
foreach ($requiredEntry in $requiredEntries) {
    if (-not $entryNameSet.Contains($requiredEntry)) {
        throw "Portable archive is missing '$requiredEntry'."
    }
}
foreach ($entryName in $entryNames) {
    if (-not $entryName.StartsWith(
        "$packageRootName/",
        [System.StringComparison]::Ordinal
    )) {
        throw "Portable archive has more than one package root: '$entryName'."
    }
}

$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    'cursorpeek-portable-test-' + [System.Guid]::NewGuid().ToString('N')
)
[System.IO.Directory]::CreateDirectory($testRoot) | Out-Null

try {
    $archiveStream = [System.IO.File]::OpenRead($resolvedPackage)
    try {
        $archive = [System.IO.Compression.ZipArchive]::new(
            $archiveStream,
            [System.IO.Compression.ZipArchiveMode]::Read,
            $false
        )
        try {
            foreach ($entry in $archive.Entries) {
                $destination = Assert-ChildPath `
                    $testRoot `
                    (Join-Path $testRoot ($entry.FullName.Replace('/', '\')))
                $parent = [System.IO.Path]::GetDirectoryName($destination)
                [System.IO.Directory]::CreateDirectory($parent) | Out-Null
                $input = $entry.Open()
                try {
                    $output = [System.IO.File]::Open(
                        $destination,
                        [System.IO.FileMode]::CreateNew,
                        [System.IO.FileAccess]::Write,
                        [System.IO.FileShare]::None
                    )
                    try {
                        $input.CopyTo($output)
                    }
                    finally {
                        $output.Dispose()
                    }
                }
                finally {
                    $input.Dispose()
                }
            }
        }
        finally {
            $archive.Dispose()
        }
    }
    finally {
        $archiveStream.Dispose()
    }

    $extractedRoot = Join-Path $testRoot $packageRootName
    $checksumPath = Join-Path $extractedRoot 'SHA256SUMS.txt'
    $checksumLines = @(
        (Read-StrictUtf8 $checksumPath).Split("`n") |
            Where-Object { $_.Length -gt 0 }
    )
    $checksumPaths = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    $lastChecksumPath = ''
    foreach ($line in $checksumLines) {
        $match = [Regex]::Match(
            $line,
            '^([0-9A-F]{64})  ([^\\:\r\n]+)$',
            [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
        )
        if (-not $match.Success) {
            throw "Internal checksum manifest has an invalid record: '$line'."
        }
        $relativePath = $match.Groups[2].Value
        if ($relativePath.StartsWith('/') -or
            @($relativePath.Split('/')) -contains '..' -or
            @($relativePath.Split('/')) -contains '.') {
            throw "Internal checksum manifest has an unsafe path '$relativePath'."
        }
        if (-not [string]::IsNullOrEmpty($lastChecksumPath) -and
            [string]::CompareOrdinal($lastChecksumPath, $relativePath) -ge 0) {
            throw 'Internal checksum records are not in strict ordinal path order.'
        }
        $lastChecksumPath = $relativePath
        if (-not $checksumPaths.Add($relativePath)) {
            throw "Internal checksum manifest repeats '$relativePath'."
        }
        $filePath = Assert-ChildPath `
            $extractedRoot `
            (Join-Path $extractedRoot ($relativePath.Replace('/', '\')))
        if (-not [System.IO.File]::Exists($filePath)) {
            throw "Internal checksum identifies a missing file '$relativePath'."
        }
        if ((Get-Sha256Hex $filePath) -cne $match.Groups[1].Value) {
            throw "Internal checksum does not match '$relativePath'."
        }
    }

    $packagedFiles = @(
        Get-ChildItem -LiteralPath $extractedRoot -Recurse -File |
            Where-Object { $_.FullName -cne $checksumPath }
    )
    if ($checksumPaths.Count -ne $packagedFiles.Count) {
        throw 'Internal checksum manifest does not cover every packaged file exactly once.'
    }
    foreach ($file in $packagedFiles) {
        $relativePath = Get-RelativePackagePath $extractedRoot $file.FullName
        if (-not $checksumPaths.Contains($relativePath)) {
            throw "Internal checksum manifest omits '$relativePath'."
        }
    }

    $metadataPath = Join-Path $extractedRoot 'RELEASE-METADATA.json'
    $metadata = (Read-StrictUtf8 $metadataPath) | ConvertFrom-Json
    if ([int] $metadata.schema_version -ne 1 -or
        [string] $metadata.product -cne 'CursorPeek' -or
        [string] $metadata.version -cne $version -or
        [string] $metadata.package_kind -cne 'portable' -or
        [string] $metadata.target -cne 'x86_64-pc-windows-msvc' -or
        [string] $metadata.architecture -cne 'x64') {
        throw 'Portable release metadata does not identify the expected product and target.'
    }
    if ([string] $metadata.source_revision -notmatch '^[0-9a-f]{40}$') {
        throw 'Portable release metadata has an invalid source revision.'
    }
    if ([bool] $metadata.source_dirty -and -not $AllowDirtyMetadata) {
        throw 'Portable release metadata identifies a dirty source tree.'
    }
    if ([int] $metadata.third_party_packages -lt 1) {
        throw 'Portable release metadata does not record third-party packages.'
    }

    $executable = Join-Path $extractedRoot 'CursorPeek.exe'
    $executableItem = Get-Item -LiteralPath $executable
    if ([string] $metadata.executable.path -cne 'CursorPeek.exe' -or
        [long] $metadata.executable.bytes -ne $executableItem.Length -or
        [string] $metadata.executable.sha256 -cne (Get-Sha256Hex $executable)) {
        throw 'Portable release metadata does not match CursorPeek.exe.'
    }

    $noticeText = Read-StrictUtf8 (Join-Path $extractedRoot 'THIRD-PARTY-NOTICES.txt')
    $noticeCount = [Regex]::Matches(
        $noticeText,
        '(?m)^Package: ',
        [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
    ).Count
    $licenseDirectory = Join-Path $extractedRoot 'licenses/third-party'
    $licensePackageCount = @(
        Get-ChildItem -LiteralPath $licenseDirectory -Directory
    ).Count
    if ($noticeCount -ne [int] $metadata.third_party_packages -or
        $licensePackageCount -ne [int] $metadata.third_party_packages) {
        throw 'Third-party notices and license directories do not cover the release graph.'
    }

    $marker = Get-Item -LiteralPath (Join-Path $extractedRoot 'CursorPeek.portable')
    if ($marker.Length -ne 0) {
        throw 'Portable marker must be an empty regular file.'
    }
    if ([System.IO.File]::Exists((Join-Path $extractedRoot 'config.ini'))) {
        throw 'Portable archive must not contain a pre-created config.ini.'
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
        throw 'Could not resolve current-user shell folders for the isolation check.'
    }
    $installedConfig = Join-Path $localAppData 'CursorPeek/config.ini'
    $uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\CursorPeek'
    $runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    $startMenuShortcut = Join-Path $startMenu 'Programs\CursorPeek\CursorPeek.lnk'
    $startMenuUninstall = Join-Path $startMenu 'Programs\CursorPeek\Uninstall CursorPeek.lnk'
    $desktopShortcut = Join-Path $desktop 'CursorPeek.lnk'
    $installedBefore = Get-OptionalFileSnapshot $installedConfig
    $startMenuBefore = Get-OptionalFileSnapshot $startMenuShortcut
    $startMenuUninstallBefore = Get-OptionalFileSnapshot $startMenuUninstall
    $desktopBefore = Get-OptionalFileSnapshot $desktopShortcut
    $startupBefore = Get-OptionalRegistryValueSnapshot $runKey 'CursorPeek'
    $registrationBefore = Get-OptionalRegistryValueSnapshot $uninstallKey 'DisplayVersion'

    $initialExecutable = Join-Path $extractedRoot 'CursorPeek.exe'
    $settingsOutput = Invoke-CursorPeek `
        $initialExecutable `
        '--settings-diagnostics' `
        $extractedRoot
    if ($settingsOutput -cne (
        'Settings storage diagnostic completed: mode=portable, configuration_created=yes'
    )) {
        throw "Portable storage diagnostic returned an unexpected result: '$settingsOutput'."
    }
    $portableConfig = Join-Path $extractedRoot 'config.ini'
    $configText = Read-StrictUtf8 $portableConfig
    if (-not $configText.StartsWith("# CursorPeek settings`n") -or
        -not $configText.Contains("dwell_delay_ms=250`n") -or
        -not $configText.Contains("start_with_windows=false`n")) {
        throw 'Portable configuration does not contain canonical defaults.'
    }
    $preservedConfig = $configText.Replace(
        "dwell_delay_ms=250`n",
        "dwell_delay_ms=700`n"
    ) + "future_portable_test=preserved`n"
    [System.IO.File]::WriteAllText(
        $portableConfig,
        $preservedConfig,
        $utf8WithoutBom
    )
    $configBeforeRelocation = Get-OptionalFileSnapshot $portableConfig

    $relocatedRoot = Join-Path $testRoot 'Relocated CursorPeek'
    [System.IO.Directory]::Move($extractedRoot, $relocatedRoot)
    $relocatedExecutable = Join-Path $relocatedRoot 'CursorPeek.exe'
    $relocatedConfig = Join-Path $relocatedRoot 'config.ini'

    $versionOutput = Invoke-CursorPeek `
        $relocatedExecutable `
        '--version' `
        $relocatedRoot
    if ($versionOutput -cne "CursorPeek $version") {
        throw "Relocated executable returned an unexpected version: '$versionOutput'."
    }

    $relocatedSettingsOutput = Invoke-CursorPeek `
        $relocatedExecutable `
        '--settings-diagnostics' `
        $relocatedRoot
    if ($relocatedSettingsOutput -cne (
        'Settings storage diagnostic completed: mode=portable, configuration_created=yes'
    )) {
        throw (
            'Relocated portable storage diagnostic returned an unexpected result: ' +
            "'$relocatedSettingsOutput'."
        )
    }
    Assert-SameSnapshot `
        $configBeforeRelocation `
        (Get-OptionalFileSnapshot $relocatedConfig) `
        'Portable configuration'

    $workerOutput = Invoke-CursorPeek `
        $relocatedExecutable `
        '--worker-diagnostics' `
        $relocatedRoot `
        -TimeoutSeconds 60
    if (-not $workerOutput.StartsWith(
        'Contained worker diagnostic completed:',
        [System.StringComparison]::Ordinal
    ) -or
        -not $workerOutput.Contains('reuse=yes') -or
        -not $workerOutput.Contains('idle_restart=yes') -or
        -not $workerOutput.Contains('session_recycle=yes')) {
        throw "Relocated worker diagnostic returned an unexpected result: '$workerOutput'."
    }

    $installedAfter = Get-OptionalFileSnapshot $installedConfig
    Assert-SameSnapshot `
        $installedBefore `
        $installedAfter `
        'Installed-mode configuration'
    Assert-SameSnapshot `
        $startMenuBefore `
        (Get-OptionalFileSnapshot $startMenuShortcut) `
        'Installed Start Menu shortcut'
    Assert-SameSnapshot `
        $startMenuUninstallBefore `
        (Get-OptionalFileSnapshot $startMenuUninstall) `
        'Installed uninstall shortcut'
    Assert-SameSnapshot `
        $desktopBefore `
        (Get-OptionalFileSnapshot $desktopShortcut) `
        'Installed desktop shortcut'
    Assert-SameRegistrySnapshot `
        $startupBefore `
        (Get-OptionalRegistryValueSnapshot $runKey 'CursorPeek') `
        'Installed startup registration'
    Assert-SameRegistrySnapshot `
        $registrationBefore `
        (Get-OptionalRegistryValueSnapshot $uninstallKey 'DisplayVersion') `
        'Installed uninstall registration'

    Start-Sleep -Milliseconds 100
    $residual = @(
        Get-CimInstance Win32_Process -Filter "Name = 'CursorPeek.exe'" -ErrorAction Stop |
            Where-Object {
                -not [string]::IsNullOrWhiteSpace($_.ExecutablePath) -and
                [string]::Equals(
                    [System.IO.Path]::GetFullPath($_.ExecutablePath),
                    [System.IO.Path]::GetFullPath($relocatedExecutable),
                    [System.StringComparison]::OrdinalIgnoreCase
                )
            }
    )
    if ($residual.Count -ne 0) {
        throw 'Relocated portable smoke left CursorPeek processes running.'
    }

    Write-Output (
        (
            "Portable package smoke passed: version={0}, entries={1}, dependencies={2}, " +
            "configured_relocation=yes, installed_state_unchanged=yes, " +
            "worker_recovery=yes, sha256={3}"
        ) -f
        $version,
        $entryNames.Count,
        [int] $metadata.third_party_packages,
        $archiveHash
    )
    if ($KeepExtracted) {
        Write-Output "Retained extracted package at '$testRoot'."
    }
}
finally {
    if (-not $KeepExtracted -and
        -not [string]::IsNullOrWhiteSpace($testRoot) -and
        [System.IO.Directory]::Exists($testRoot)) {
        [System.IO.Directory]::Delete($testRoot, $true)
    }
}
