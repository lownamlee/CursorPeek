[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $PortablePackage,

    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $OutputDirectory = 'target/packages',

    [Parameter()]
    [string] $NsisCompiler = '',

    [Parameter()]
    [switch] $AllowDirtyMetadata
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.IO.Compression

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$installerScript = Join-Path $repoRoot 'packaging/CursorPeek.nsi'
$iconPath = Join-Path $repoRoot 'assets/windows/CursorPeek.ico'
$resolvedPortable = (Resolve-Path -LiteralPath $PortablePackage).Path
$portableSidecar = "$resolvedPortable.sha256"
$resolvedOutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
$utf8Strict = [System.Text.UTF8Encoding]::new($false, $true)
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$nsisVersion = '3.12'

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

function Write-CanonicalText {
    param(
        [Parameter(Mandatory = $true)][string] $LiteralPath,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string] $Text
    )

    $canonical = $Text.Replace("`r`n", "`n").Replace("`r", "`n")
    $parent = [System.IO.Path]::GetDirectoryName($LiteralPath)
    if (-not [string]::IsNullOrWhiteSpace($parent)) {
        [System.IO.Directory]::CreateDirectory($parent) | Out-Null
    }
    [System.IO.File]::WriteAllText($LiteralPath, $canonical, $utf8WithoutBom)
}

function Copy-CanonicalText {
    param(
        [Parameter(Mandatory = $true)][string] $Source,
        [Parameter(Mandatory = $true)][string] $Destination
    )

    $bytes = [System.IO.File]::ReadAllBytes($Source)
    $text = $utf8Strict.GetString($bytes)
    if ($text.Length -gt 0 -and $text[0] -eq [char] 0xFEFF) {
        $text = $text.Substring(1)
    }
    Write-CanonicalText $Destination $text
}

function Assert-ChildPath {
    param(
        [Parameter(Mandatory = $true)][string] $Root,
        [Parameter(Mandatory = $true)][string] $Candidate,
        [Parameter(Mandatory = $true)][string] $Label
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
        throw "$Label escapes its expected root: '$resolvedCandidate'."
    }
    return $resolvedCandidate
}

function Assert-SafeArchivePath {
    param(
        [Parameter(Mandatory = $true)][string] $Path,
        [Parameter(Mandatory = $true)][string] $ExpectedRoot
    )

    if ([string]::IsNullOrWhiteSpace($Path) -or
        $Path.Contains('\') -or
        $Path.StartsWith('/') -or
        $Path.Contains(':')) {
        throw "Portable archive contains an unsafe path '$Path'."
    }
    $segments = @($Path.Split('/'))
    if ($segments.Count -lt 2 -or $segments[0] -cne $ExpectedRoot) {
        throw "Portable archive entry is outside '$ExpectedRoot': '$Path'."
    }
    foreach ($segment in $segments) {
        if ([string]::IsNullOrWhiteSpace($segment) -or
            $segment -ceq '.' -or
            $segment -ceq '..') {
            throw "Portable archive contains an unsafe path segment in '$Path'."
        }
    }
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
        throw "Payload file escapes its staging root: '$resolvedPath'."
    }
    return $resolvedPath.Substring($rootPrefix.Length).Replace('\', '/')
}

function ConvertTo-NsisLiteral {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string] $Value)

    if ($Value.Contains('"') -or $Value.Contains("`r") -or $Value.Contains("`n")) {
        throw "A generated NSIS value contains unsupported quoting: '$Value'."
    }
    return $Value.Replace('$', '$$')
}

function Get-OrdinalSortedStrings {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [string[]] $Values
    )

    $copy = [string[]] $Values.Clone()
    [System.Array]::Sort($copy, [System.StringComparer]::Ordinal)
    return $copy
}

if (-not [System.IO.File]::Exists($portableSidecar)) {
    throw "Portable checksum sidecar is missing: '$portableSidecar'."
}
if (-not [System.IO.File]::Exists($installerScript) -or
    -not [System.IO.File]::Exists($iconPath)) {
    throw 'Installer source or the approved CursorPeek icon is missing.'
}

$archiveName = [System.IO.Path]::GetFileName($resolvedPortable)
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

$sidecarText = Read-StrictUtf8 $portableSidecar
$sidecarMatch = [Regex]::Match(
    $sidecarText,
    '^([0-9A-F]{64})  ([^/\r\n]+)\n$',
    [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
)
if (-not $sidecarMatch.Success -or $sidecarMatch.Groups[2].Value -cne $archiveName) {
    throw 'Portable checksum sidecar has an invalid canonical record.'
}
$portableHash = Get-Sha256Hex $resolvedPortable
if ($portableHash -cne $sidecarMatch.Groups[1].Value) {
    throw 'Portable archive does not match its SHA-256 sidecar.'
}

$metadataText = & cargo metadata `
    --locked `
    --no-deps `
    --format-version 1 `
    --manifest-path (Join-Path $repoRoot 'Cargo.toml')
if ($LASTEXITCODE -ne 0) {
    throw 'Cargo could not read the product version for installer packaging.'
}
$cargoMetadata = $metadataText | ConvertFrom-Json
$rootPackage = @(
    $cargoMetadata.packages |
        Where-Object { [string] $_.name -ceq 'windows-cursorpeek' }
)
if ($rootPackage.Count -ne 1 -or [string] $rootPackage[0].version -cne $version) {
    throw "Portable version '$version' does not match the current Cargo package."
}

$versionMatch = [Regex]::Match(
    $version,
    '^([0-9]+)\.([0-9]+)\.([0-9]+)(?:[-+].*)?$',
    [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
)
if (-not $versionMatch.Success) {
    throw "Version '$version' cannot be represented in Windows version resources."
}
$versionParts = for ($index = 1; $index -le 3; $index++) {
    $part = [uint32]::Parse(
        $versionMatch.Groups[$index].Value,
        [System.Globalization.CultureInfo]::InvariantCulture
    )
    if ($part -gt 65535) {
        throw "Version component '$part' exceeds the Windows resource limit."
    }
    $part
}
$fileVersion = "$($versionParts[0]).$($versionParts[1]).$($versionParts[2]).0"

$compilerOutput = @(
    & (Join-Path $PSScriptRoot 'Get-Nsis.ps1') `
        -DestinationDirectory (Join-Path $repoRoot 'target/tools')
)
if ($compilerOutput.Count -ne 1) {
    throw 'The pinned NSIS resolver did not return exactly one compiler path.'
}
$verifiedCompiler = (Resolve-Path -LiteralPath $compilerOutput[0]).Path
if (-not [string]::IsNullOrWhiteSpace($NsisCompiler)) {
    $requestedCompiler = (Resolve-Path -LiteralPath $NsisCompiler).Path
    if (-not [string]::Equals(
        $requestedCompiler,
        $verifiedCompiler,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'NsisCompiler must identify the archive-verified pinned NSIS distribution.'
    }
}
$resolvedCompiler = $verifiedCompiler
$compilerVersion = (& $resolvedCompiler /VERSION 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $compilerVersion -cne "v$nsisVersion") {
    throw "Installer packaging requires NSIS v$nsisVersion, not '$compilerVersion'."
}
$nsisLicensePath = Join-Path ([System.IO.Path]::GetDirectoryName($resolvedCompiler)) 'COPYING'
if (-not [System.IO.File]::Exists($nsisLicensePath)) {
    throw 'The authenticated NSIS distribution does not contain its COPYING file.'
}

[System.IO.Directory]::CreateDirectory($resolvedOutputDirectory) | Out-Null
$installerName = "CursorPeek-$version-windows-x64-setup.exe"
$sidecarName = "$installerName.sha256"
$finalInstaller = Join-Path $resolvedOutputDirectory $installerName
$finalSidecar = Join-Path $resolvedOutputDirectory $sidecarName
foreach ($output in @($finalInstaller, $finalSidecar)) {
    if ([System.IO.File]::Exists($output) -or [System.IO.Directory]::Exists($output)) {
        throw "Refusing to overwrite existing installer output '$output'."
    }
}

$stagingRoot = Join-Path $resolvedOutputDirectory (
    '.cursorpeek-installer-' + [System.Guid]::NewGuid().ToString('N')
)
$extractionRoot = Join-Path $stagingRoot 'portable'
$payloadRoot = Join-Path $extractionRoot $packageRootName
$installInclude = Join-Path $stagingRoot 'InstallFiles.nsh'
$uninstallInclude = Join-Path $stagingRoot 'UninstallFiles.nsh'
$temporaryInstaller = Join-Path $stagingRoot $installerName
$temporarySidecar = Join-Path $stagingRoot $sidecarName
$publishedInstaller = $false
$publishedSidecar = $false
[System.IO.Directory]::CreateDirectory($extractionRoot) | Out-Null

try {
    $entryNames = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $archiveStream = [System.IO.File]::OpenRead($resolvedPortable)
    try {
        $archive = [System.IO.Compression.ZipArchive]::new(
            $archiveStream,
            [System.IO.Compression.ZipArchiveMode]::Read,
            $false
        )
        try {
            foreach ($entry in $archive.Entries) {
                $entryName = [string] $entry.FullName
                Assert-SafeArchivePath $entryName $packageRootName
                if (-not $entryNames.Add($entryName)) {
                    throw "Portable archive repeats a case-colliding entry '$entryName'."
                }
                $destination = Assert-ChildPath `
                    $extractionRoot `
                    (Join-Path $extractionRoot ($entryName.Replace('/', '\'))) `
                    'Portable extraction'
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

    foreach ($requiredRelativePath in @(
        'CursorPeek.exe',
        'CursorPeek.portable',
        'CHANGELOG.md',
        'README.txt',
        'RELEASE-METADATA.json',
        'SHA256SUMS.txt',
        'THIRD-PARTY-NOTICES.txt',
        'docs/KNOWN_LIMITATIONS.md',
        'docs/PRIVACY.md',
        'docs/SECURITY.md',
        'docs/THREAT_MODEL.md',
        'docs/USER_GUIDE.md',
        'licenses/LICENSE-APACHE',
        'licenses/LICENSE-MIT'
    )) {
        $requiredPath = Join-Path $payloadRoot ($requiredRelativePath.Replace('/', '\'))
        if (-not [System.IO.File]::Exists($requiredPath)) {
            throw "Portable archive is missing '$requiredRelativePath'."
        }
    }
    if (([System.IO.FileInfo] (Join-Path $payloadRoot 'CursorPeek.portable')).Length -ne 0) {
        throw 'Portable marker must be an empty regular file.'
    }

    $checksumPath = Join-Path $payloadRoot 'SHA256SUMS.txt'
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
            throw "Portable checksum manifest has an invalid record '$line'."
        }
        $checkedFile = Assert-ChildPath `
            $payloadRoot `
            (Join-Path $payloadRoot ($match.Groups[2].Value.Replace('/', '\'))) `
            'Portable checksum'
        if (-not [System.IO.File]::Exists($checkedFile) -or
            (Get-Sha256Hex $checkedFile) -cne $match.Groups[1].Value) {
            throw "Portable checksum does not match '$($match.Groups[2].Value)'."
        }
    }
    $portableFiles = @(
        Get-ChildItem -LiteralPath $payloadRoot -Recurse -File |
            Where-Object { $_.FullName -cne $checksumPath }
    )
    if ($checksumPaths.Count -ne $portableFiles.Count) {
        throw 'Portable checksums do not cover every payload file exactly once.'
    }
    foreach ($file in $portableFiles) {
        if (-not $checksumPaths.Contains((Get-RelativePath $payloadRoot $file.FullName))) {
            throw "Portable checksums omit '$($file.FullName)'."
        }
    }

    $portableMetadataPath = Join-Path $payloadRoot 'RELEASE-METADATA.json'
    $portableMetadata = (Read-StrictUtf8 $portableMetadataPath) | ConvertFrom-Json
    if ([string] $portableMetadata.product -cne 'CursorPeek' -or
        [string] $portableMetadata.version -cne $version -or
        [string] $portableMetadata.package_kind -cne 'portable' -or
        [string] $portableMetadata.target -cne 'x86_64-pc-windows-msvc' -or
        [string] $portableMetadata.architecture -cne 'x64') {
        throw 'Portable metadata does not identify the expected CursorPeek package.'
    }
    if ([string] $portableMetadata.source_revision -notmatch '^[0-9a-f]{40}$' -or
        [int] $portableMetadata.third_party_packages -lt 1) {
        throw 'Portable metadata has invalid source or dependency information.'
    }
    if ([bool] $portableMetadata.source_dirty -and -not $AllowDirtyMetadata) {
        throw 'Portable metadata identifies a dirty source tree.'
    }
    $portableExecutable = Join-Path $payloadRoot 'CursorPeek.exe'
    $portableExecutableItem = Get-Item -LiteralPath $portableExecutable
    if ([string] $portableMetadata.executable.path -cne 'CursorPeek.exe' -or
        [long] $portableMetadata.executable.bytes -ne $portableExecutableItem.Length -or
        [string] $portableMetadata.executable.sha256 -cne
            (Get-Sha256Hex $portableExecutable)) {
        throw 'Portable metadata does not match CursorPeek.exe.'
    }

    foreach ($portableOnly in @(
        'CursorPeek.portable',
        'README.txt',
        'RELEASE-METADATA.json',
        'SHA256SUMS.txt'
    )) {
        [System.IO.File]::Delete((Join-Path $payloadRoot $portableOnly))
    }

    Copy-CanonicalText `
        $nsisLicensePath `
        (Join-Path $payloadRoot 'licenses/packaging/NSIS-COPYING')
    $noticesPath = Join-Path $payloadRoot 'THIRD-PARTY-NOTICES.txt'
    $notices = Read-StrictUtf8 $noticesPath
    $nsisNotice = @"

Packaging technology: NSIS $nsisVersion
License: zlib/libpng, bzip2, and Common Public License 1.0 with the documented LZMA linking exception
Source: https://sourceforge.net/projects/nsis/files/NSIS%203/$nsisVersion/
License file: licenses/packaging/NSIS-COPYING
"@
    Write-CanonicalText $noticesPath ($notices.TrimEnd("`n") + "$nsisNotice`n")

    $installedReadme = @"
CursorPeek $version

CursorPeek is installed for the current Windows user.

Run CursorPeek from the Start Menu or CursorPeek.exe. Hover over a supported
local file in File Explorer and keep the pointer still. The notification-area
icon provides pause, preview-size, delay, theme, startup, and exit controls.

No administrator rights or separate runtime is required. User settings are
stored in %LOCALAPPDATA%\CursorPeek\config.ini. The uninstaller can either
remove that configuration or preserve it for a later reinstall.

The binary may be unsigned. Read docs\KNOWN_LIMITATIONS.md before testing and
docs\USER_GUIDE.md for supported formats, privacy, limits, and troubleshooting.
"@
    Write-CanonicalText (Join-Path $payloadRoot 'README.txt') "$installedReadme`n"

    $installedExecutable = Join-Path $payloadRoot 'CursorPeek.exe'
    $installerMetadata = [ordered] @{
        schema_version = 1
        product = 'CursorPeek'
        version = $version
        package_kind = 'installer'
        target = [string] $portableMetadata.target
        architecture = 'x64'
        rust_toolchain = [string] $portableMetadata.rust_toolchain
        source_revision = [string] $portableMetadata.source_revision
        source_dirty = [bool] $portableMetadata.source_dirty
        executable = [ordered] @{
            path = 'CursorPeek.exe'
            bytes = ([System.IO.FileInfo] $installedExecutable).Length
            sha256 = Get-Sha256Hex $installedExecutable
        }
        third_party_packages = [int] $portableMetadata.third_party_packages
        packager = [ordered] @{
            name = 'NSIS'
            version = $nsisVersion
            license_file = 'licenses/packaging/NSIS-COPYING'
        }
        portable_archive_sha256 = $portableHash
    }
    Write-CanonicalText `
        (Join-Path $payloadRoot 'RELEASE-METADATA.json') `
        (($installerMetadata | ConvertTo-Json -Depth 6) + "`n")

    $payloadFilesWithoutChecksums = @(
        Get-ChildItem -LiteralPath $payloadRoot -Recurse -File |
            Where-Object { $_.Name -cne 'SHA256SUMS.txt' }
    )
    $relativeFiles = Get-OrdinalSortedStrings @(
        $payloadFilesWithoutChecksums |
            ForEach-Object { Get-RelativePath $payloadRoot $_.FullName }
    )
    $checksumText = (
        $relativeFiles |
            ForEach-Object {
                $file = Join-Path $payloadRoot ($_.Replace('/', '\'))
                "$(Get-Sha256Hex $file)  $_"
            }
    ) -join "`n"
    Write-CanonicalText (Join-Path $payloadRoot 'SHA256SUMS.txt') "$checksumText`n"

    $payloadFiles = Get-OrdinalSortedStrings @(
        Get-ChildItem -LiteralPath $payloadRoot -Recurse -File |
            ForEach-Object { Get-RelativePath $payloadRoot $_.FullName }
    )
    if ($payloadFiles.Count -lt 10 -or
        $payloadFiles -contains 'CursorPeek.portable') {
        throw 'Installer payload construction produced an invalid file set.'
    }

    $installLines = [System.Collections.Generic.List[string]]::new()
    $currentDirectory = $null
    foreach ($relativePath in $payloadFiles) {
        $directory = [System.IO.Path]::GetDirectoryName(
            $relativePath.Replace('/', '\')
        )
        if ($directory -cne $currentDirectory) {
            if ([string]::IsNullOrEmpty($directory)) {
                $installLines.Add('SetOutPath "$INSTDIR"')
            }
            else {
                $escapedDirectory = ConvertTo-NsisLiteral $directory
                $installLines.Add(('SetOutPath "$INSTDIR\{0}"' -f $escapedDirectory))
            }
            $currentDirectory = $directory
        }
        $source = Join-Path $payloadRoot ($relativePath.Replace('/', '\'))
        $source = ConvertTo-NsisLiteral ([System.IO.Path]::GetFullPath($source))
        $name = ConvertTo-NsisLiteral ([System.IO.Path]::GetFileName($relativePath))
        $installLines.Add(('File "/oname={0}" "{1}"' -f $name, $source))
    }
    Write-CanonicalText $installInclude (($installLines -join "`n") + "`n")

    $uninstallLines = [System.Collections.Generic.List[string]]::new()
    $uninstallFiles = [string[]] $payloadFiles.Clone()
    [System.Array]::Reverse($uninstallFiles)
    foreach ($relativePath in $uninstallFiles) {
        $escapedPath = ConvertTo-NsisLiteral ($relativePath.Replace('/', '\'))
        $uninstallLines.Add(('Delete "$INSTDIR\{0}"' -f $escapedPath))
    }
    $directories = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($relativePath in $payloadFiles) {
        $directory = [System.IO.Path]::GetDirectoryName(
            $relativePath.Replace('/', '\')
        )
        while (-not [string]::IsNullOrEmpty($directory)) {
            $directories.Add($directory) | Out-Null
            $directory = [System.IO.Path]::GetDirectoryName($directory)
        }
    }
    $orderedDirectories = Get-OrdinalSortedStrings @($directories)
    [System.Array]::Reverse($orderedDirectories)
    $maximumDepth = 0
    foreach ($directory in $orderedDirectories) {
        $maximumDepth = [Math]::Max($maximumDepth, @($directory.Split('\')).Count)
    }
    for ($depth = $maximumDepth; $depth -ge 1; $depth--) {
        foreach ($directory in $orderedDirectories) {
            if (@($directory.Split('\')).Count -eq $depth) {
                $escapedDirectory = ConvertTo-NsisLiteral $directory
                $uninstallLines.Add(('RMDir "$INSTDIR\{0}"' -f $escapedDirectory))
            }
        }
    }
    Write-CanonicalText $uninstallInclude (($uninstallLines -join "`n") + "`n")

    $payloadBytes = (
        Get-ChildItem -LiteralPath $payloadRoot -Recurse -File |
            Measure-Object -Property Length -Sum
    ).Sum
    $estimatedSizeKiB = [Math]::Max(
        1,
        [Math]::Ceiling(([double] $payloadBytes + 524288) / 1024)
    )

    $arguments = @(
        '/V3',
        '/WX',
        '/NOCONFIG',
        '/NOCD',
        "/DPRODUCT_VERSION=$version",
        "/DPRODUCT_FILE_VERSION=$fileVersion",
        "/DPRODUCT_ICON=$([System.IO.Path]::GetFullPath($iconPath))",
        "/DOUTPUT_FILE=$temporaryInstaller",
        "/DINSTALL_FILES_INCLUDE=$installInclude",
        "/DUNINSTALL_FILES_INCLUDE=$uninstallInclude",
        "/DESTIMATED_SIZE_KIB=$estimatedSizeKiB",
        $installerScript
    )
    $previousSourceDateEpoch = [Environment]::GetEnvironmentVariable(
        'SOURCE_DATE_EPOCH',
        [EnvironmentVariableTarget]::Process
    )
    try {
        [Environment]::SetEnvironmentVariable(
            'SOURCE_DATE_EPOCH',
            '0',
            [EnvironmentVariableTarget]::Process
        )
        $previousErrorActionPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = 'Continue'
            $compilerLog = @(& $resolvedCompiler @arguments 2>&1)
            $compilerExitCode = $LASTEXITCODE
        }
        finally {
            $ErrorActionPreference = $previousErrorActionPreference
        }
        if ($compilerExitCode -ne 0) {
            throw "NSIS compilation failed:`n$($compilerLog -join "`n")"
        }
    }
    finally {
        [Environment]::SetEnvironmentVariable(
            'SOURCE_DATE_EPOCH',
            $previousSourceDateEpoch,
            [EnvironmentVariableTarget]::Process
        )
    }
    if (-not [System.IO.File]::Exists($temporaryInstaller)) {
        throw 'NSIS reported success without creating the installer.'
    }

    $installerHash = Get-Sha256Hex $temporaryInstaller
    Write-CanonicalText $temporarySidecar "$installerHash  $installerName`n"
    [System.IO.File]::Move($temporaryInstaller, $finalInstaller)
    $publishedInstaller = $true
    [System.IO.File]::Move($temporarySidecar, $finalSidecar)
    $publishedSidecar = $true

    Write-Output (
        (
            "Installer package created: version={0}, payload_files={1}, " +
            "nsis={2}, sha256={3}, path={4}"
        ) -f
        $version,
        $payloadFiles.Count,
        $nsisVersion,
        $installerHash,
        $finalInstaller
    )
}
catch {
    if ($publishedSidecar -and [System.IO.File]::Exists($finalSidecar)) {
        [System.IO.File]::Delete($finalSidecar)
    }
    if ($publishedInstaller -and [System.IO.File]::Exists($finalInstaller)) {
        [System.IO.File]::Delete($finalInstaller)
    }
    throw
}
finally {
    if ([System.IO.Directory]::Exists($stagingRoot)) {
        [System.IO.Directory]::Delete($stagingRoot, $true)
    }
}
