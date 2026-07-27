[CmdletBinding()]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $Executable = 'target/release/CursorPeek.exe',

    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $OutputDirectory = 'target/packages',

    [Parameter()]
    [switch] $AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.IO.Compression

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifestPath = Join-Path $repoRoot 'Cargo.toml'
$toolchainPath = Join-Path $repoRoot 'rust-toolchain.toml'
$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
$resolvedOutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
$utf8Strict = [System.Text.UTF8Encoding]::new($false, $true)
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$fixedZipTimestamp = [System.DateTimeOffset]::new(
    1980,
    1,
    1,
    0,
    0,
    0,
    [System.TimeSpan]::Zero
)

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

function Assert-SafeSegment {
    param(
        [Parameter(Mandatory = $true)][string] $Value,
        [Parameter(Mandatory = $true)][string] $Label
    )

    if ($Value -notmatch '^[A-Za-z0-9][A-Za-z0-9._+-]*$') {
        throw "$Label contains characters that are unsafe in a package path: '$Value'."
    }
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
        throw "Package input escapes the staging root: '$resolvedPath'."
    }
    return $resolvedPath.Substring($rootPrefix.Length).Replace('\', '/')
}

function Sort-RecordsByOrdinalPath {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]] $Records,
        [Parameter(Mandatory = $true)][string] $Property
    )

    $recordByPath = [System.Collections.Generic.Dictionary[string, object]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($record in $Records) {
        $path = [string] $record.$Property
        if ([string]::IsNullOrWhiteSpace($path) -or $recordByPath.ContainsKey($path)) {
            throw "Package records contain an empty or repeated $Property value '$path'."
        }
        $recordByPath.Add($path, $record)
    }

    $paths = [string[]]::new($recordByPath.Count)
    $recordByPath.Keys.CopyTo($paths, 0)
    [System.Array]::Sort($paths, [System.StringComparer]::Ordinal)
    foreach ($path in $paths) {
        $recordByPath[$path]
    }
}

function Invoke-GitText {
    param([Parameter(Mandatory = $true)][string[]] $Arguments)

    $output = & git -C $repoRoot @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Git failed while running: git -C `"$repoRoot`" $($Arguments -join ' ')"
    }
    return @($output)
}

function Get-ReleaseGraph {
    $metadataText = & cargo metadata `
        --locked `
        --format-version 1 `
        --filter-platform x86_64-pc-windows-msvc `
        --manifest-path $manifestPath
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not resolve the locked Windows release dependency graph.'
    }
    $metadata = $metadataText | ConvertFrom-Json

    $packageById = [System.Collections.Generic.Dictionary[string, object]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($package in $metadata.packages) {
        $packageById.Add([string] $package.id, $package)
    }

    $nodeById = [System.Collections.Generic.Dictionary[string, object]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($node in $metadata.resolve.nodes) {
        $nodeById.Add([string] $node.id, $node)
    }

    $rootId = [string] $metadata.resolve.root
    if ([string]::IsNullOrWhiteSpace($rootId) -or -not $nodeById.ContainsKey($rootId)) {
        throw 'Cargo metadata did not identify the root CursorPeek package.'
    }

    $seen = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    $queue = [System.Collections.Generic.Queue[string]]::new()
    $queue.Enqueue($rootId)
    while ($queue.Count -gt 0) {
        $id = $queue.Dequeue()
        if (-not $seen.Add($id)) {
            continue
        }
        $node = $nodeById[$id]
        foreach ($dependency in @($node.deps)) {
            $include = @($dependency.dep_kinds).Count -eq 0
            foreach ($kind in @($dependency.dep_kinds)) {
                if ([string] $kind.kind -cne 'dev') {
                    $include = $true
                }
            }
            if ($include) {
                $queue.Enqueue([string] $dependency.pkg)
            }
        }
    }

    $workspaceIds = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($workspaceId in $metadata.workspace_members) {
        $workspaceIds.Add([string] $workspaceId) | Out-Null
    }

    $thirdParty = @(
        foreach ($id in $seen) {
            if (-not $workspaceIds.Contains($id)) {
                $packageById[$id]
            }
        }
    ) | Sort-Object name, version

    return [PSCustomObject] @{
        Root = $packageById[$rootId]
        Packages = @($thirdParty)
        PackageCount = $seen.Count
    }
}

function Get-PackageLicenseFiles {
    param([Parameter(Mandatory = $true)][object] $Package)

    $packageRoot = [System.IO.Path]::GetDirectoryName([string] $Package.manifest_path)
    $paths = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )

    foreach ($candidate in Get-ChildItem -LiteralPath $packageRoot -File) {
        if ($candidate.Name -match '^(?i:license|licence|copying|notice|unlicense)') {
            $paths.Add($candidate.FullName) | Out-Null
        }
    }

    $declaredLicenseFile = [string] $Package.license_file
    if (-not [string]::IsNullOrWhiteSpace($declaredLicenseFile)) {
        $declaredPath = if ([System.IO.Path]::IsPathRooted($declaredLicenseFile)) {
            $declaredLicenseFile
        }
        else {
            Join-Path $packageRoot $declaredLicenseFile
        }
        $declaredPath = Assert-ChildPath $packageRoot $declaredPath 'Declared license file'
        if (-not [System.IO.File]::Exists($declaredPath)) {
            throw "Declared license file is missing: '$declaredPath'."
        }
        $paths.Add($declaredPath) | Out-Null
    }

    if ($paths.Count -eq 0) {
        throw "Dependency $($Package.name) $($Package.version) has no distributable license file."
    }

    return @(
        $paths |
            ForEach-Object { Get-Item -LiteralPath $_ } |
            Sort-Object Name, FullName
    )
}

function New-DeterministicZip {
    param(
        [Parameter(Mandatory = $true)][string] $ContentRoot,
        [Parameter(Mandatory = $true)][string] $ArchivePath
    )

    $unsortedFiles = @(
        Get-ChildItem -LiteralPath $ContentRoot -Recurse -File |
            ForEach-Object {
                [PSCustomObject] @{
                    File = $_
                    Entry = Get-RelativePackagePath `
                        ([System.IO.Path]::GetDirectoryName($ContentRoot)) `
                        $_.FullName
                }
            }
    )
    $files = @(Sort-RecordsByOrdinalPath $unsortedFiles 'Entry')
    if ($files.Count -eq 0) {
        throw 'Refusing to create an empty portable archive.'
    }

    $archiveStream = [System.IO.File]::Open(
        $ArchivePath,
        [System.IO.FileMode]::CreateNew,
        [System.IO.FileAccess]::ReadWrite,
        [System.IO.FileShare]::None
    )
    try {
        $archive = [System.IO.Compression.ZipArchive]::new(
            $archiveStream,
            [System.IO.Compression.ZipArchiveMode]::Create,
            $false
        )
        try {
            foreach ($file in $files) {
                $entry = $archive.CreateEntry(
                    [string] $file.Entry,
                    [System.IO.Compression.CompressionLevel]::NoCompression
                )
                $entry.LastWriteTime = $fixedZipTimestamp
                $input = [System.IO.File]::OpenRead($file.File.FullName)
                try {
                    $output = $entry.Open()
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
}

if (-not [System.IO.File]::Exists($resolvedExecutable)) {
    throw "Release executable does not exist: '$resolvedExecutable'."
}
if ([System.IO.Path]::GetExtension($resolvedExecutable) -cne '.exe') {
    throw "Portable input must be a Windows .exe file: '$resolvedExecutable'."
}

$revisionLines = @(Invoke-GitText @('rev-parse', '--verify', 'HEAD'))
if ($revisionLines.Count -ne 1) {
    throw 'Git returned an unexpected number of source revisions.'
}
$revision = $revisionLines[0].Trim()
if ($revision -notmatch '^[0-9a-f]{40}$') {
    throw "Git returned an invalid source revision: '$revision'."
}
$dirtyLines = @(Invoke-GitText @('status', '--porcelain=v1', '--untracked-files=normal'))
$sourceDirty = $dirtyLines.Count -gt 0
if ($sourceDirty -and -not $AllowDirty) {
    throw 'The source tree is not clean. Commit the package inputs or pass -AllowDirty for a non-release test.'
}

$graph = Get-ReleaseGraph
if ([string] $graph.Root.name -cne 'windows-cursorpeek') {
    throw "Unexpected root package '$($graph.Root.name)'."
}
$version = [string] $graph.Root.version
Assert-SafeSegment $version 'Package version'
if (@($graph.Packages).Count -eq 0) {
    throw 'The Windows release graph unexpectedly contains no third-party packages.'
}

$toolchainMatch = [Regex]::Match(
    [System.IO.File]::ReadAllText($toolchainPath, [System.Text.Encoding]::UTF8),
    '(?m)^\s*channel\s*=\s*"([^"]+)"\s*$',
    [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
)
if (-not $toolchainMatch.Success) {
    throw 'rust-toolchain.toml does not declare one pinned channel.'
}
$rustToolchain = $toolchainMatch.Groups[1].Value
Assert-SafeSegment $rustToolchain 'Rust toolchain'

[System.IO.Directory]::CreateDirectory($resolvedOutputDirectory) | Out-Null
$archiveName = "CursorPeek-$version-windows-x64-portable.zip"
$sidecarName = "$archiveName.sha256"
$finalArchive = Join-Path $resolvedOutputDirectory $archiveName
$finalSidecar = Join-Path $resolvedOutputDirectory $sidecarName
foreach ($output in @($finalArchive, $finalSidecar)) {
    if ([System.IO.File]::Exists($output) -or [System.IO.Directory]::Exists($output)) {
        throw "Refusing to overwrite existing package output '$output'."
    }
}

$stagingRoot = Join-Path $resolvedOutputDirectory (
    '.cursorpeek-package-' + [System.Guid]::NewGuid().ToString('N')
)
[System.IO.Directory]::CreateDirectory($stagingRoot) | Out-Null
$packageRootName = "CursorPeek-$version"
$contentRoot = Join-Path $stagingRoot $packageRootName
[System.IO.Directory]::CreateDirectory($contentRoot) | Out-Null
$temporaryArchive = Join-Path $stagingRoot $archiveName
$temporarySidecar = Join-Path $stagingRoot $sidecarName
$publishedArchive = $false
$publishedSidecar = $false

try {
    [System.IO.File]::Copy(
        $resolvedExecutable,
        (Join-Path $contentRoot 'CursorPeek.exe'),
        $false
    )
    [System.IO.File]::WriteAllBytes(
        (Join-Path $contentRoot 'CursorPeek.portable'),
        [byte[]] @()
    )

    $portableReadme = @"
CursorPeek $version portable

1. Extract the complete CursorPeek-$version folder to a writable local directory.
2. Run CursorPeek.exe.
3. Hover over a supported local file in File Explorer and keep the pointer still.
4. Use the notification-area icon to pause previews, change settings, or exit.

CursorPeek.portable keeps config.ini beside the executable. Move the marker and
configuration together with CursorPeek.exe when relocating this copy.

CursorPeek does not require installation or administrator rights. It does not add
itself to PATH. Start with Windows is changed only when you explicitly enable that
tray setting.

The binary may be unsigned. Verify the archive with its adjacent .sha256 file.
Read docs/KNOWN_LIMITATIONS.md before testing and docs/USER_GUIDE.md for supported
formats, limits, privacy, and troubleshooting.
"@
    Write-CanonicalText (Join-Path $contentRoot 'README.txt') "$portableReadme`n"

    $projectFiles = [ordered] @{
        'CHANGELOG.md' = Join-Path $repoRoot 'CHANGELOG.md'
        'docs/KNOWN_LIMITATIONS.md' = Join-Path $repoRoot 'docs/KNOWN_LIMITATIONS.md'
        'docs/USER_GUIDE.md' = Join-Path $repoRoot 'docs/USER_GUIDE.md'
        'docs/PRIVACY.md' = Join-Path $repoRoot 'PRIVACY.md'
        'docs/SECURITY.md' = Join-Path $repoRoot 'SECURITY.md'
        'docs/THREAT_MODEL.md' = Join-Path $repoRoot 'THREAT_MODEL.md'
        'licenses/LICENSE-MIT' = Join-Path $repoRoot 'LICENSE-MIT'
        'licenses/LICENSE-APACHE' = Join-Path $repoRoot 'LICENSE-APACHE'
    }
    foreach ($entry in $projectFiles.GetEnumerator()) {
        Copy-CanonicalText `
            $entry.Value `
            (Join-Path $contentRoot ($entry.Key.Replace('/', '\')))
    }

    $notices = [System.Text.StringBuilder]::new()
    $notices.AppendLine('CursorPeek third-party software notices') | Out-Null
    $notices.AppendLine('=======================================') | Out-Null
    $notices.AppendLine() | Out-Null
    $notices.AppendLine(
        'This portable package contains software linked into CursorPeek or used by its build.'
    ) | Out-Null
    $notices.AppendLine(
        'The exact upstream license files for every locked package follow under licenses/third-party.'
    ) | Out-Null
    $notices.AppendLine() | Out-Null

    foreach ($package in @($graph.Packages)) {
        $name = [string] $package.name
        $packageVersion = [string] $package.version
        $license = [string] $package.license
        Assert-SafeSegment $name 'Dependency name'
        Assert-SafeSegment $packageVersion 'Dependency version'
        if ([string]::IsNullOrWhiteSpace($license) -or $license -match "[`r`n]") {
            throw "Dependency $name $packageVersion has invalid license metadata."
        }

        $packageDirectory = "$name-$packageVersion"
        $licenseFiles = @(Get-PackageLicenseFiles $package)
        $licenseNames = [System.Collections.Generic.HashSet[string]]::new(
            [System.StringComparer]::OrdinalIgnoreCase
        )
        $noticePaths = [System.Collections.Generic.List[string]]::new()
        foreach ($licenseFile in $licenseFiles) {
            if (-not $licenseNames.Add($licenseFile.Name)) {
                throw "Dependency $name $packageVersion has colliding license filenames."
            }
            $relativeDestination = (
                "licenses/third-party/{0}/{1}" -f $packageDirectory, $licenseFile.Name
            )
            Copy-CanonicalText `
                $licenseFile.FullName `
                (Join-Path $contentRoot ($relativeDestination.Replace('/', '\')))
            $noticePaths.Add($relativeDestination)
        }

        $notices.AppendLine("Package: $name $packageVersion") | Out-Null
        $notices.AppendLine("License: $license") | Out-Null
        $notices.AppendLine(
            "Source: https://crates.io/crates/$name/$packageVersion"
        ) | Out-Null
        $notices.AppendLine(
            "License files: $($noticePaths -join ', ')"
        ) | Out-Null
        $notices.AppendLine() | Out-Null
    }
    Write-CanonicalText `
        (Join-Path $contentRoot 'THIRD-PARTY-NOTICES.txt') `
        $notices.ToString()

    $packagedExecutable = Join-Path $contentRoot 'CursorPeek.exe'
    $releaseMetadata = [ordered] @{
        schema_version = 1
        product = 'CursorPeek'
        version = $version
        package_kind = 'portable'
        target = 'x86_64-pc-windows-msvc'
        architecture = 'x64'
        rust_toolchain = $rustToolchain
        source_revision = $revision
        source_dirty = [bool] $sourceDirty
        executable = [ordered] @{
            path = 'CursorPeek.exe'
            bytes = ([System.IO.FileInfo] $packagedExecutable).Length
            sha256 = Get-Sha256Hex $packagedExecutable
        }
        third_party_packages = @($graph.Packages).Count
    }
    Write-CanonicalText `
        (Join-Path $contentRoot 'RELEASE-METADATA.json') `
        (($releaseMetadata | ConvertTo-Json -Depth 5) + "`n")

    $unsortedChecksumEntries = @(
        Get-ChildItem -LiteralPath $contentRoot -Recurse -File |
            Where-Object { $_.Name -cne 'SHA256SUMS.txt' } |
            ForEach-Object {
                [PSCustomObject] @{
                    Path = Get-RelativePackagePath $contentRoot $_.FullName
                    Hash = Get-Sha256Hex $_.FullName
                }
            }
    )
    $checksumEntries = @(
        Sort-RecordsByOrdinalPath $unsortedChecksumEntries 'Path'
    )
    $checksumText = (
        $checksumEntries |
            ForEach-Object { "$($_.Hash)  $($_.Path)" }
    ) -join "`n"
    Write-CanonicalText `
        (Join-Path $contentRoot 'SHA256SUMS.txt') `
        "$checksumText`n"

    New-DeterministicZip $contentRoot $temporaryArchive
    $archiveHash = Get-Sha256Hex $temporaryArchive
    Write-CanonicalText $temporarySidecar "$archiveHash  $archiveName`n"

    [System.IO.File]::Move($temporaryArchive, $finalArchive)
    $publishedArchive = $true
    [System.IO.File]::Move($temporarySidecar, $finalSidecar)
    $publishedSidecar = $true

    Write-Output (
        (
            "Portable package created: version={0}, files={1}, dependencies={2}, " +
            "sha256={3}, path={4}"
        ) -f
        $version,
        (Get-ChildItem -LiteralPath $contentRoot -Recurse -File).Count,
        @($graph.Packages).Count,
        $archiveHash,
        $finalArchive
    )
}
catch {
    if ($publishedSidecar -and [System.IO.File]::Exists($finalSidecar)) {
        [System.IO.File]::Delete($finalSidecar)
    }
    if ($publishedArchive -and [System.IO.File]::Exists($finalArchive)) {
        [System.IO.File]::Delete($finalArchive)
    }
    throw
}
finally {
    if ([System.IO.Directory]::Exists($stagingRoot)) {
        [System.IO.Directory]::Delete($stagingRoot, $true)
    }
}
