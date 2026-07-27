[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$')]
    [string] $Version,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$')]
    [string] $Tag,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$')]
    [string] $Repository,

    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $ArtifactsDirectory = 'target/release-assets',

    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $ChangelogPath = 'CHANGELOG.md',

    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $OutputPath = 'target/release-notes.md',

    [Parameter()]
    [ValidatePattern('^$|^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$')]
    [string] $PreviousTag = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)

function Resolve-RepositoryPath {
    param([Parameter(Mandatory = $true)][string] $Path)

    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

function Get-Sha256Hex {
    param([Parameter(Mandatory = $true)][string] $LiteralPath)

    return (
        Get-FileHash -Algorithm SHA256 -LiteralPath $LiteralPath
    ).Hash.ToUpperInvariant()
}

if ($Tag -cne "v$Version") {
    throw "Release tag '$Tag' does not match version '$Version'."
}

$resolvedArtifacts = Resolve-RepositoryPath $ArtifactsDirectory
$resolvedChangelog = Resolve-RepositoryPath $ChangelogPath
$resolvedOutput = Resolve-RepositoryPath $OutputPath

if (-not [System.IO.Directory]::Exists($resolvedArtifacts)) {
    throw "Release artifacts directory does not exist: '$resolvedArtifacts'."
}
if (-not [System.IO.File]::Exists($resolvedChangelog)) {
    throw "Changelog does not exist: '$resolvedChangelog'."
}

$artifactPrefix = $resolvedArtifacts.TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
) + [System.IO.Path]::DirectorySeparatorChar
if ([string]::Equals(
    $resolvedOutput,
    $resolvedArtifacts,
    [System.StringComparison]::OrdinalIgnoreCase
) -or $resolvedOutput.StartsWith(
    $artifactPrefix,
    [System.StringComparison]::OrdinalIgnoreCase
)) {
    throw 'Release notes must remain outside the canonical release artifacts directory.'
}

$installerName = "CursorPeek-$Version-windows-x64-setup.exe"
$portableName = "CursorPeek-$Version-windows-x64-portable.zip"
$sbomName = "CursorPeek-$Version.cdx.json"
$checksumsName = 'SHA256SUMS.txt'
$assetNames = @($installerName, $portableName, $sbomName)

$assetHashes = @{}
foreach ($name in $assetNames) {
    $path = Join-Path $resolvedArtifacts $name
    if (-not [System.IO.File]::Exists($path)) {
        throw "Release artifact is missing: '$path'."
    }
    $assetHashes[$name] = Get-Sha256Hex $path
}

$checksumsPath = Join-Path $resolvedArtifacts $checksumsName
if (-not [System.IO.File]::Exists($checksumsPath)) {
    throw "Release checksum manifest is missing: '$checksumsPath'."
}
$checksumEntries = @{}
foreach ($line in Get-Content -LiteralPath $checksumsPath) {
    if ($line -notmatch '^(?<hash>[0-9a-fA-F]{64})  (?<name>[^\r\n]+)$') {
        throw "Malformed release checksum entry: '$line'."
    }
    if ($checksumEntries.ContainsKey($Matches['name'])) {
        throw "Duplicate release checksum entry: '$($Matches['name'])'."
    }
    $checksumEntries[$Matches['name']] = $Matches['hash'].ToUpperInvariant()
}
if ($checksumEntries.Count -ne $assetNames.Count) {
    throw 'The checksum manifest does not describe the canonical three release artifacts.'
}
foreach ($name in $assetNames) {
    if (-not $checksumEntries.ContainsKey($name) -or
        $checksumEntries[$name] -cne $assetHashes[$name]) {
        throw "The checksum manifest does not match '$name'."
    }
}

$changelog = Get-Content -Raw -LiteralPath $resolvedChangelog
$escapedVersion = [regex]::Escape($Version)
$releaseSection = [regex]::Match(
    $changelog,
    "(?ms)^## \[$escapedVersion\] - \d{4}-\d{2}-\d{2}\r?\n" +
        "(?<body>.*?)(?=^## |\z)"
)
if (-not $releaseSection.Success) {
    throw "The changelog has no release section for '$Version'."
}
$highlights = $releaseSection.Groups['body'].Value.Trim()
if ([string]::IsNullOrWhiteSpace($highlights)) {
    throw "The '$Version' changelog section is empty."
}

if ([string]::IsNullOrWhiteSpace($PreviousTag)) {
    try {
        $resolvedPreviousTag = @(
            & git -C $repoRoot describe --tags --abbrev=0 "$Tag^" 2>$null
        )
    }
    catch {
        throw "Could not resolve the release preceding '$Tag'."
    }
    if ($LASTEXITCODE -ne 0 -or $resolvedPreviousTag.Count -ne 1) {
        throw "Could not resolve the release preceding '$Tag'."
    }
    $PreviousTag = $resolvedPreviousTag[0].Trim()
}
if ($PreviousTag -notmatch
    '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' -or
    $PreviousTag -ceq $Tag) {
    throw "Previous release tag is invalid: '$PreviousTag'."
}

$downloadBase = "https://github.com/$Repository/releases/download/$Tag"
$compareUrl = "https://github.com/$Repository/compare/$PreviousTag...$Tag"
$notes = @(
    '## Downloads and hashes'
    ''
    '| Description | Filename | SHA256 hash |'
    '| --- | --- | --- |'
    (
        '| Per-user installer - x64 | [{0}]({1}/{0}) | `{2}` |' -f
        $installerName,
        $downloadBase,
        $assetHashes[$installerName]
    )
    (
        '| Portable - x64 | [{0}]({1}/{0}) | `{2}` |' -f
        $portableName,
        $downloadBase,
        $assetHashes[$portableName]
    )
    ''
    '## Highlights'
    ''
    $highlights
    ''
    '## Verification'
    ''
    "- [SHA256SUMS.txt]($downloadBase/$checksumsName)"
    "- [CycloneDX SBOM]($downloadBase/$sbomName)"
    '- GitHub build-provenance and SBOM attestations are created before publication.'
    '- The installer is not code-signed yet. Verify its SHA256 hash before running it.'
    ''
    "[Compare changes since $PreviousTag]($compareUrl)"
) -join "`n"
$notes += "`n"

$outputDirectory = [System.IO.Path]::GetDirectoryName($resolvedOutput)
[System.IO.Directory]::CreateDirectory($outputDirectory) | Out-Null
$temporaryOutput = "$resolvedOutput.$([Guid]::NewGuid().ToString('N')).tmp"
try {
    [System.IO.File]::WriteAllText($temporaryOutput, $notes, $utf8WithoutBom)
    if ([System.IO.File]::Exists($resolvedOutput)) {
        [System.IO.File]::Replace($temporaryOutput, $resolvedOutput, $null)
    }
    else {
        [System.IO.File]::Move($temporaryOutput, $resolvedOutput)
    }
}
finally {
    if ([System.IO.File]::Exists($temporaryOutput)) {
        [System.IO.File]::Delete($temporaryOutput)
    }
}

Write-Output (
    "Release notes created: version={0}, previous={1}, path={2}" -f
    $Version,
    $PreviousTag,
    $resolvedOutput
)
