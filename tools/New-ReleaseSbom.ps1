[CmdletBinding()]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $OutputPath = 'target/security/CursorPeek.cdx.json'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$expectedGeneratorVersion = '0.5.9'
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifestPath = Join-Path $repoRoot 'Cargo.toml'
$lockPath = Join-Path $repoRoot 'Cargo.lock'
$rawPath = Join-Path $repoRoot 'CursorPeek_bin.cdx.json'
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

function Get-CanonicalPackageUrl {
    param([Parameter(Mandatory = $true)][string] $PackageUrl)

    $match = [Regex]::Match(
        $PackageUrl,
        '^(pkg:cargo/[^?]+)\?download_url=file:[^#]*(#.*)?$',
        [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
    )
    if (-not $match.Success) {
        throw "Workspace component has an unexpected local package URL: '$PackageUrl'."
    }
    return "$($match.Groups[1].Value)$($match.Groups[2].Value)"
}

function Convert-WorkspaceComponent {
    param(
        [Parameter(Mandatory = $true)][object] $Component,
        [Parameter(Mandatory = $true)]
        [System.Collections.Generic.HashSet[string]] $WorkspacePackageUrls,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.Dictionary[string, string]] $ReferenceMap
    )

    $referenceProperty = $Component.PSObject.Properties['bom-ref']
    if ($null -ne $referenceProperty -and
        ([string] $referenceProperty.Value).StartsWith(
            'path+file:',
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
        $purlProperty = $Component.PSObject.Properties['purl']
        if ($null -eq $purlProperty) {
            throw 'A local workspace component does not contain a package URL.'
        }
        $canonical = Get-CanonicalPackageUrl ([string] $purlProperty.Value)
        $packageUrl = $canonical.Split('#')[0]
        if (-not $WorkspacePackageUrls.Contains($packageUrl)) {
            throw "SBOM contains an unexpected local workspace package: '$packageUrl'."
        }

        $original = [string] $referenceProperty.Value
        if ($ReferenceMap.ContainsKey($original)) {
            throw "SBOM repeats local workspace reference '$original'."
        }
        $ReferenceMap.Add($original, $canonical)
        $referenceProperty.Value = $canonical
        $purlProperty.Value = $canonical
    }

    $childrenProperty = $Component.PSObject.Properties['components']
    if ($null -ne $childrenProperty) {
        foreach ($child in @($childrenProperty.Value)) {
            Convert-WorkspaceComponent $child $WorkspacePackageUrls $ReferenceMap
        }
    }
}

function Add-ComponentReferences {
    param(
        [Parameter(Mandatory = $true)][object] $Component,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.HashSet[string]] $References
    )

    $referenceProperty = $Component.PSObject.Properties['bom-ref']
    if ($null -eq $referenceProperty -or
        [string]::IsNullOrWhiteSpace([string] $referenceProperty.Value)) {
        throw 'Every SBOM component must contain a nonempty bom-ref.'
    }
    if (-not $References.Add([string] $referenceProperty.Value)) {
        throw "SBOM contains duplicate bom-ref '$($referenceProperty.Value)'."
    }

    $childrenProperty = $Component.PSObject.Properties['components']
    if ($null -ne $childrenProperty) {
        foreach ($child in @($childrenProperty.Value)) {
            Add-ComponentReferences $child $References
        }
    }
}

$versionOutput = (& cargo cyclonedx --version).Trim()
if ($LASTEXITCODE -ne 0) {
    throw 'Could not run cargo-cyclonedx.'
}
if ($versionOutput -cne "cargo-cyclonedx-cyclonedx $expectedGeneratorVersion") {
    throw (
        "Expected cargo-cyclonedx $expectedGeneratorVersion; found '$versionOutput'. " +
        "Install it with: cargo install cargo-cyclonedx --version " +
        "$expectedGeneratorVersion --locked"
    )
}

if ([System.IO.File]::Exists($rawPath)) {
    throw "Refusing to overwrite unexpected generator output '$rawPath'."
}
if ([string]::Equals(
    $rawPath,
    $resolvedOutput,
    [System.StringComparison]::OrdinalIgnoreCase
)) {
    throw 'OutputPath must not target the temporary cargo-cyclonedx output.'
}

$metadataText = & cargo metadata --locked --format-version 1 --no-deps
if ($LASTEXITCODE -ne 0) {
    throw 'Could not read locked Cargo workspace metadata.'
}
$metadata = $metadataText | ConvertFrom-Json
$workspaceIds = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
foreach ($workspaceId in $metadata.workspace_members) {
    $workspaceIds.Add([string] $workspaceId) | Out-Null
}
$workspacePackageUrls = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
foreach ($package in $metadata.packages) {
    if ($workspaceIds.Contains([string] $package.id)) {
        $workspacePackageUrls.Add(
            "pkg:cargo/$($package.name)@$($package.version)"
        ) | Out-Null
    }
}
if ($workspacePackageUrls.Count -eq 0) {
    throw 'Cargo metadata did not contain any workspace packages.'
}

$lockHashBefore = Get-Sha256Hex $lockPath
$hadSourceDateEpoch = Test-Path Env:SOURCE_DATE_EPOCH
$previousSourceDateEpoch = $env:SOURCE_DATE_EPOCH
try {
    $env:SOURCE_DATE_EPOCH = '0'
    Push-Location $repoRoot
    try {
        & cargo cyclonedx `
            --manifest-path $manifestPath `
            --format json `
            --describe binaries `
            --target x86_64-pc-windows-msvc `
            --spec-version 1.5
        if ($LASTEXITCODE -ne 0) {
            throw "cargo-cyclonedx failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        Pop-Location
    }

    if (-not [System.IO.File]::Exists($rawPath)) {
        throw "cargo-cyclonedx did not create '$rawPath'."
    }
    if ((Get-Sha256Hex $lockPath) -cne $lockHashBefore) {
        throw 'cargo-cyclonedx changed Cargo.lock.'
    }

    $document = [System.IO.File]::ReadAllText($rawPath, [System.Text.Encoding]::UTF8) |
        ConvertFrom-Json
    if ([string] $document.bomFormat -cne 'CycloneDX' -or
        [string] $document.specVersion -cne '1.5' -or
        [int] $document.version -ne 1) {
        throw 'Generator output is not a CycloneDX 1.5 version-1 document.'
    }
    if ([string] $document.metadata.timestamp -cne '1970-01-01T00:00:00.000000000Z') {
        throw "Generator output has a non-reproducible timestamp '$($document.metadata.timestamp)'."
    }

    $referenceMap = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::Ordinal
    )
    Convert-WorkspaceComponent `
        $document.metadata.component `
        $workspacePackageUrls `
        $referenceMap
    foreach ($component in @($document.components)) {
        Convert-WorkspaceComponent $component $workspacePackageUrls $referenceMap
    }
    if ($referenceMap.Count -ne $workspacePackageUrls.Count) {
        throw (
            "Expected $($workspacePackageUrls.Count) workspace references; " +
            "sanitized $($referenceMap.Count)."
        )
    }

    foreach ($dependency in @($document.dependencies)) {
        $reference = [string] $dependency.ref
        if ($referenceMap.ContainsKey($reference)) {
            $dependency.ref = $referenceMap[$reference]
        }
        $dependsOnProperty = $dependency.PSObject.Properties['dependsOn']
        if ($null -ne $dependsOnProperty) {
            $dependsOnProperty.Value = @(
                foreach ($dependentReference in @($dependsOnProperty.Value)) {
                    $value = [string] $dependentReference
                    if ($referenceMap.ContainsKey($value)) {
                        $referenceMap[$value]
                    }
                    else {
                        $value
                    }
                }
            )
        }
    }

    $componentReferences = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    Add-ComponentReferences $document.metadata.component $componentReferences
    foreach ($component in @($document.components)) {
        Add-ComponentReferences $component $componentReferences
    }
    foreach ($dependency in @($document.dependencies)) {
        if (-not $componentReferences.Contains([string] $dependency.ref)) {
            throw "Dependency ref '$($dependency.ref)' does not identify a component."
        }
        $dependsOnProperty = $dependency.PSObject.Properties['dependsOn']
        if ($null -ne $dependsOnProperty) {
            foreach ($dependentReference in @($dependsOnProperty.Value)) {
                if (-not $componentReferences.Contains([string] $dependentReference)) {
                    throw "Dependency target '$dependentReference' does not identify a component."
                }
            }
        }
    }

    $json = $document | ConvertTo-Json -Depth 100
    foreach ($pattern in @(
        '(?i)path\+file:',
        '(?i)download_url=file:',
        '(?i)file://',
        '(?i)(?:^|["\s])[a-z]:[\\/]',
        '(?i)/(?:home|users)/[^/]+/'
    )) {
        if ($json -match $pattern) {
            throw "Sanitized SBOM still contains forbidden local-path pattern '$pattern'."
        }
    }

    $outputDirectory = [System.IO.Path]::GetDirectoryName($resolvedOutput)
    if (-not [string]::IsNullOrWhiteSpace($outputDirectory)) {
        [System.IO.Directory]::CreateDirectory($outputDirectory) | Out-Null
    }
    [System.IO.File]::WriteAllText($resolvedOutput, "$json`n", $utf8WithoutBom)

    $hash = Get-Sha256Hex $resolvedOutput
    Write-Output (
        "CycloneDX SBOM generated: components={0}, dependencies={1}, sha256={2}, path={3}" -f
        $componentReferences.Count,
        @($document.dependencies).Count,
        $hash,
        $resolvedOutput
    )
}
finally {
    if ($hadSourceDateEpoch) {
        $env:SOURCE_DATE_EPOCH = $previousSourceDateEpoch
    }
    else {
        Remove-Item Env:SOURCE_DATE_EPOCH -ErrorAction SilentlyContinue
    }
    if ([System.IO.File]::Exists($rawPath)) {
        [System.IO.File]::Delete($rawPath)
    }
}
