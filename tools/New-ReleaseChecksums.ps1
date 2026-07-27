[CmdletBinding()]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $ArtifactsDirectory = 'target/release-assets',

    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $OutputPath = 'target/release-assets/SHA256SUMS.txt'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifestPath = Join-Path $repoRoot 'Cargo.toml'
$resolvedArtifacts = [System.IO.Path]::GetFullPath($ArtifactsDirectory)
$resolvedOutput = [System.IO.Path]::GetFullPath($OutputPath)
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)

function Get-Sha256Hex {
    param([Parameter(Mandatory = $true)][string] $LiteralPath)

    $stream = [System.IO.File]::OpenRead($LiteralPath)
    try {
        $algorithm = [System.Security.Cryptography.SHA256]::Create()
        try {
            return [System.BitConverter]::ToString(
                $algorithm.ComputeHash($stream)
            ).Replace('-', '').ToLowerInvariant()
        }
        finally {
            $algorithm.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

if (-not [System.IO.Directory]::Exists($resolvedArtifacts)) {
    throw "Release artifacts directory does not exist: '$resolvedArtifacts'."
}
if (-not [string]::Equals(
    [System.IO.Path]::GetDirectoryName($resolvedOutput),
    $resolvedArtifacts,
    [System.StringComparison]::OrdinalIgnoreCase
)) {
    throw 'The checksum manifest must be written directly inside the release artifacts directory.'
}

$metadataText = & cargo metadata `
    --manifest-path $manifestPath `
    --locked `
    --format-version 1 `
    --no-deps
if ($LASTEXITCODE -ne 0) {
    throw 'Could not read locked Cargo workspace metadata.'
}
$metadata = $metadataText | ConvertFrom-Json
$package = @(
    $metadata.packages |
        Where-Object { [string] $_.name -ceq 'windows-cursorpeek' }
)
if ($package.Count -ne 1) {
    throw 'Cargo metadata must contain exactly one windows-cursorpeek package.'
}
$version = [string] $package[0].version
if ($version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    throw "Cargo package version is not a stable semantic version: '$version'."
}

[string[]] $expectedNames = @(
    "CursorPeek-$version.cdx.json"
    "CursorPeek-$version-windows-x64-portable.zip"
    "CursorPeek-$version-windows-x64-setup.exe"
)
[System.Array]::Sort($expectedNames, [System.StringComparer]::Ordinal)
$expectedSet = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
foreach ($name in $expectedNames) {
    $expectedSet.Add($name) | Out-Null
}

$entries = @(Get-ChildItem -LiteralPath $resolvedArtifacts -Force)
$unexpectedEntries = @(
    foreach ($entry in $entries) {
        if ([string]::Equals(
            $entry.FullName,
            $resolvedOutput,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
            continue
        }
        if ($entry.PSIsContainer -or
            ($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
            -not $expectedSet.Contains($entry.Name)) {
            $entry.Name
        }
    }
)
if ($unexpectedEntries.Count -ne 0) {
    throw "Release directory contains unexpected entries: $($unexpectedEntries -join ', ')."
}

[string[]] $actualNames = @(
    $entries |
        Where-Object {
            -not $_.PSIsContainer -and
            -not [string]::Equals(
                $_.FullName,
                $resolvedOutput,
                [System.StringComparison]::OrdinalIgnoreCase
            )
        } |
        ForEach-Object Name
)
[System.Array]::Sort($actualNames, [System.StringComparer]::Ordinal)
if ($actualNames.Count -ne $expectedNames.Count) {
    throw "Expected $($expectedNames.Count) release artifacts; found $($actualNames.Count)."
}
for ($index = 0; $index -lt $expectedNames.Count; $index++) {
    if ($actualNames[$index] -cne $expectedNames[$index]) {
        throw (
            "Release artifact set differs at index ${index}: " +
            "'$($actualNames[$index])' instead of '$($expectedNames[$index])'."
        )
    }
}

$lines = @(
    foreach ($name in $expectedNames) {
        $path = Join-Path $resolvedArtifacts $name
        if (-not [System.IO.File]::Exists($path)) {
            throw "Release artifact is missing: '$path'."
        }
        "$(Get-Sha256Hex $path)  $name"
    }
)
$text = ($lines -join "`n") + "`n"
$temporaryOutput = "$resolvedOutput.$([Guid]::NewGuid().ToString('N')).tmp"
try {
    [System.IO.File]::WriteAllText($temporaryOutput, $text, $utf8WithoutBom)
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
    "Release checksums created: version={0}, artifacts={1}, sha256={2}, path={3}" -f
    $version,
    $expectedNames.Count,
    (Get-Sha256Hex $resolvedOutput),
    $resolvedOutput
)
