[CmdletBinding()]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $DestinationDirectory = 'target/tools'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$nsisVersion = '3.12'
$archiveSha256 = '56581F90DB321581C5381193D796FFFCF2D24B2F8FED2160A6C6A3BAA67F2C4F'
$downloadPageUri = (
    'https://sourceforge.net/projects/nsis/files/NSIS%203/{0}/nsis-{0}.zip/download' -f
    $nsisVersion
)
$mirrorHost = 'downloads.sourceforge.net'
$mirrorPath = "/project/nsis/NSIS%203/$nsisVersion/nsis-$nsisVersion.zip"
$maximumRedirectPageBytes = 512KB
$resolvedDestination = [System.IO.Path]::GetFullPath($DestinationDirectory)
$archiveDirectory = Join-Path $resolvedDestination 'nsis-download'
$archivePath = Join-Path $archiveDirectory "nsis-$nsisVersion.zip"
$distributionRoot = Join-Path $resolvedDestination "nsis-$nsisVersion"
$nsisRoot = Join-Path $distributionRoot "nsis-$nsisVersion"
$compilerPath = Join-Path $nsisRoot 'makensis.exe'
$stampPath = Join-Path $distributionRoot '.cursorpeek-nsis-sha256'
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

function Get-Sha256Stream {
    param([Parameter(Mandatory = $true)][System.IO.Stream] $Stream)

    $algorithm = [System.Security.Cryptography.SHA256]::Create()
    try {
        return [System.BitConverter]::ToString(
            $algorithm.ComputeHash($Stream)
        ).Replace('-', '')
    }
    finally {
        $algorithm.Dispose()
    }
}

function Get-SourceForgeMirrorUri {
    param([Parameter(Mandatory = $true)][string] $LiteralPath)

    $file = [System.IO.FileInfo] $LiteralPath
    if ($file.Length -gt $maximumRedirectPageBytes) {
        throw 'The unexpected NSIS response is too large to be a mirror redirect page.'
    }

    $html = [System.IO.File]::ReadAllText($LiteralPath)
    if (-not $html.TrimStart().StartsWith(
        '<!doctype html',
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'The unexpected NSIS response is not a SourceForge redirect page.'
    }

    $redirects = [System.Collections.Generic.List[string]]::new()
    foreach ($metaMatch in [regex]::Matches($html, '(?is)<meta\b[^>]*>')) {
        $tag = $metaMatch.Value
        $httpEquiv = [regex]::Match(
            $tag,
            '(?is)\bhttp-equiv\s*=\s*(["''])(?<value>.*?)\1'
        )
        if (-not $httpEquiv.Success -or
            $httpEquiv.Groups['value'].Value -cne 'refresh') {
            continue
        }

        $content = [regex]::Match(
            $tag,
            '(?is)\bcontent\s*=\s*(["''])(?<value>.*?)\1'
        )
        if (-not $content.Success) {
            continue
        }
        $target = [regex]::Match(
            $content.Groups['value'].Value,
            '(?is)^\s*\d+\s*;\s*url=(?<url>.+?)\s*$'
        )
        if ($target.Success) {
            $redirects.Add(
                [System.Net.WebUtility]::HtmlDecode(
                    $target.Groups['url'].Value
                )
            )
        }
    }

    if ($redirects.Count -ne 1) {
        throw 'The SourceForge response did not contain exactly one mirror redirect.'
    }

    $uri = [System.Uri] $redirects[0]
    if ($uri.Scheme -cne 'https' -or
        $uri.Host -cne $mirrorHost -or
        -not $uri.IsDefaultPort -or
        $uri.UserInfo.Length -ne 0 -or
        $uri.Fragment.Length -ne 0 -or
        $uri.AbsolutePath -cne $mirrorPath) {
        throw "SourceForge returned an unexpected NSIS mirror URI '$uri'."
    }
    return $uri
}

function Save-AuthenticatedNsisArchive {
    param([Parameter(Mandatory = $true)][string] $LiteralPath)

    $initialDownload = "$LiteralPath.initial"
    try {
        Invoke-WebRequest -Uri $downloadPageUri -OutFile $initialDownload
        if ((Get-Sha256Hex $initialDownload) -ceq $archiveSha256) {
            [System.IO.File]::Move($initialDownload, $LiteralPath)
            return
        }

        $mirrorUri = Get-SourceForgeMirrorUri $initialDownload
        Invoke-WebRequest -Uri $mirrorUri -OutFile $LiteralPath
        $actualHash = Get-Sha256Hex $LiteralPath
        if ($actualHash -cne $archiveSha256) {
            throw "Downloaded NSIS has SHA-256 $actualHash; expected $archiveSha256."
        }
    }
    finally {
        if ([System.IO.File]::Exists($initialDownload)) {
            [System.IO.File]::Delete($initialDownload)
        }
        if ([System.IO.File]::Exists($LiteralPath) -and
            (Get-Sha256Hex $LiteralPath) -cne $archiveSha256) {
            [System.IO.File]::Delete($LiteralPath)
        }
    }
}

function Assert-SafeArchiveEntry {
    param([Parameter(Mandatory = $true)][string] $Entry)

    if ([string]::IsNullOrWhiteSpace($Entry) -or
        $Entry.Contains('\') -or
        $Entry.StartsWith('/') -or
        $Entry.Contains(':')) {
        throw "The NSIS archive contains an unsafe path '$Entry'."
    }
    foreach ($segment in $Entry.Split('/')) {
        if ([string]::IsNullOrWhiteSpace($segment) -or
            $segment -ceq '.' -or
            $segment -ceq '..') {
            throw "The NSIS archive contains an unsafe path segment in '$Entry'."
        }
    }
    if (-not $Entry.StartsWith(
        "nsis-$nsisVersion/",
        [System.StringComparison]::Ordinal
    )) {
        throw "The NSIS archive contains an unexpected root path '$Entry'."
    }
}

function Assert-CompilerVersion {
    param([Parameter(Mandatory = $true)][string] $LiteralPath)

    $versionOutput = (& $LiteralPath /VERSION 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $versionOutput -cne "v$nsisVersion") {
        throw "Expected NSIS v$nsisVersion, but '$LiteralPath' returned '$versionOutput'."
    }
}

function Assert-ExtractedDistribution {
    $archiveStream = [System.IO.File]::OpenRead($archivePath)
    try {
        $archive = [System.IO.Compression.ZipArchive]::new(
            $archiveStream,
            [System.IO.Compression.ZipArchiveMode]::Read,
            $false
        )
        try {
            $fileEntries = @($archive.Entries | Where-Object { $_.Name.Length -gt 0 })
            $extractedFiles = @(
                Get-ChildItem -LiteralPath $nsisRoot -Recurse -File
            )
            if ($fileEntries.Count -ne $extractedFiles.Count) {
                throw 'The cached NSIS distribution has an unexpected file count.'
            }
            foreach ($entry in $fileEntries) {
                $entryName = [string] $entry.FullName
                Assert-SafeArchiveEntry $entryName
                $relative = $entryName.Substring("nsis-$nsisVersion/".Length)
                $extracted = Join-Path $nsisRoot ($relative.Replace('/', '\'))
                if (-not [System.IO.File]::Exists($extracted)) {
                    throw "The cached NSIS distribution is missing '$relative'."
                }
                if ([long] $entry.Length -ne ([System.IO.FileInfo] $extracted).Length) {
                    throw "The cached NSIS file '$relative' has the wrong length."
                }
                $entryStream = $entry.Open()
                try {
                    $entryHash = Get-Sha256Stream $entryStream
                }
                finally {
                    $entryStream.Dispose()
                }
                if ((Get-Sha256Hex $extracted) -cne $entryHash) {
                    throw "The cached NSIS file '$relative' does not match the archive."
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

[System.IO.Directory]::CreateDirectory($archiveDirectory) | Out-Null
if ([System.IO.File]::Exists($archivePath)) {
    $actualHash = Get-Sha256Hex $archivePath
    if ($actualHash -cne $archiveSha256) {
        throw (
            "The cached NSIS archive has SHA-256 $actualHash; expected $archiveSha256. " +
            "Remove '$archivePath' before retrying."
        )
    }
}
else {
    $temporaryDownload = "$archivePath.$([System.Guid]::NewGuid().ToString('N')).download"
    try {
        Save-AuthenticatedNsisArchive $temporaryDownload
        [System.IO.File]::Move($temporaryDownload, $archivePath)
    }
    finally {
        if ([System.IO.File]::Exists($temporaryDownload)) {
            [System.IO.File]::Delete($temporaryDownload)
        }
    }
}

if ([System.IO.File]::Exists($compilerPath)) {
    Assert-ExtractedDistribution
    Assert-CompilerVersion $compilerPath
    $stamp = if ([System.IO.File]::Exists($stampPath)) {
        [System.IO.File]::ReadAllText($stampPath).Trim()
    }
    else {
        ''
    }
    if (-not [string]::IsNullOrEmpty($stamp) -and $stamp -cne $archiveSha256) {
        throw (
            "The cached NSIS distribution does not match its recorded archive. " +
            "Remove '$distributionRoot' before retrying."
        )
    }
    if ([string]::IsNullOrEmpty($stamp)) {
        [System.IO.File]::WriteAllText($stampPath, "$archiveSha256`n", $utf8WithoutBom)
    }
    Write-Output $compilerPath
    return
}

if ([System.IO.Directory]::Exists($distributionRoot)) {
    throw (
        "The cached NSIS distribution is incomplete. Remove '$distributionRoot' before retrying."
    )
}

$temporaryRoot = Join-Path $resolvedDestination (
    ".nsis-$nsisVersion-" + [System.Guid]::NewGuid().ToString('N')
)
[System.IO.Directory]::CreateDirectory($temporaryRoot) | Out-Null
try {
    $archiveStream = [System.IO.File]::OpenRead($archivePath)
    try {
        $archive = [System.IO.Compression.ZipArchive]::new(
            $archiveStream,
            [System.IO.Compression.ZipArchiveMode]::Read,
            $false
        )
        try {
            foreach ($entry in $archive.Entries) {
                Assert-SafeArchiveEntry ([string] $entry.FullName)
            }
            [System.IO.Compression.ZipFileExtensions]::ExtractToDirectory(
                $archive,
                $temporaryRoot
            )
        }
        finally {
            $archive.Dispose()
        }
    }
    finally {
        $archiveStream.Dispose()
    }

    $temporaryCompiler = Join-Path $temporaryRoot "nsis-$nsisVersion/makensis.exe"
    if (-not [System.IO.File]::Exists($temporaryCompiler)) {
        throw 'The authenticated NSIS archive does not contain makensis.exe.'
    }
    Assert-CompilerVersion $temporaryCompiler
    [System.IO.File]::WriteAllText(
        (Join-Path $temporaryRoot '.cursorpeek-nsis-sha256'),
        "$archiveSha256`n",
        $utf8WithoutBom
    )
    [System.IO.Directory]::Move($temporaryRoot, $distributionRoot)
}
finally {
    if ([System.IO.Directory]::Exists($temporaryRoot)) {
        [System.IO.Directory]::Delete($temporaryRoot, $true)
    }
}

Assert-CompilerVersion $compilerPath
Assert-ExtractedDistribution
Write-Output $compilerPath
