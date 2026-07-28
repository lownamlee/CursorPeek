[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repository = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$corpusRoot = [System.IO.Path]::GetFullPath((Join-Path $repository 'fuzz\corpus'))
$expectedPrefix = $repository.TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
) + [System.IO.Path]::DirectorySeparatorChar
if (-not $corpusRoot.StartsWith(
    $expectedPrefix,
    [System.StringComparison]::OrdinalIgnoreCase
)) {
    throw 'The fuzz corpus must remain inside the CursorPeek repository.'
}

function New-Frame {
    param(
        [Parameter(Mandatory)]
        [UInt16] $Kind,

        [Parameter(Mandatory)]
        [UInt64] $Generation,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [byte[]] $Payload,

        [UInt32] $DeclaredLength = $Payload.Length
    )

    $stream = [System.IO.MemoryStream]::new()
    $writer = [System.IO.BinaryWriter]::new($stream)
    try {
        $writer.Write([System.Text.Encoding]::ASCII.GetBytes('CPWK'))
        $writer.Write([UInt16] 7)
        $writer.Write($Kind)
        $writer.Write($DeclaredLength)
        $writer.Write([UInt32] 0)
        $writer.Write($Generation)
        $writer.Write($Payload)
        $writer.Flush()
        return [byte[]] $stream.ToArray()
    }
    finally {
        $writer.Dispose()
        $stream.Dispose()
    }
}

function New-BinaryPayload {
    param(
        [Parameter(Mandatory)]
        [scriptblock] $Write
    )

    $stream = [System.IO.MemoryStream]::new()
    $writer = [System.IO.BinaryWriter]::new($stream)
    try {
        & $Write $writer
        $writer.Flush()
        return [byte[]] $stream.ToArray()
    }
    finally {
        $writer.Dispose()
        $stream.Dispose()
    }
}

function Write-Seed {
    param(
        [Parameter(Mandatory)]
        [string] $Target,

        [Parameter(Mandatory)]
        [string] $Name,

        [Parameter(Mandatory)]
        [byte[]] $Data
    )

    if ($Target -notmatch '^[a-z_]+$' -or $Name -notmatch '^[a-z0-9-]+$') {
        throw "Invalid fuzz seed name '$Target/$Name'."
    }
    $directory = Join-Path $corpusRoot $Target
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
    $path = Join-Path $directory $Name
    [System.IO.File]::WriteAllBytes($path, $Data)
}

$nonce = [byte[]] (0..15)
$helloPayload = New-BinaryPayload {
    param($writer)
    $writer.Write($nonce)
    $writer.Write([UInt16] 128)
    $writer.Write([System.Text.Encoding]::ASCII.GetBytes('auto'))
}
$readyPayload = [byte[]] $nonce.Clone()
$resolvePayload = New-BinaryPayload {
    param($writer)
    $writer.Write([Int32]::MinValue)
    $writer.Write([Int32]::MaxValue)
}
$statusPayload = New-BinaryPayload {
    param($writer)
    $writer.Write([UInt32] 0)
    $writer.Write([UInt32] 3)
}
$statusProtocolPayload = [byte[]]::new($statusPayload.Length + 1)
[System.Array]::Copy($statusPayload, 0, $statusProtocolPayload, 1, $statusPayload.Length)
$textPayload = New-BinaryPayload {
    param($writer)
    $encoding = [System.Text.Encoding]::ASCII.GetBytes('UTF-8')
    $text = [System.Text.Encoding]::UTF8.GetBytes("hello`n")
    $writer.Write([UInt32] 1)
    $writer.Write([UInt32] 0)
    $writer.Write([UInt64] $text.Length)
    $writer.Write([UInt32] $encoding.Length)
    $writer.Write([UInt32] $text.Length)
    $writer.Write($encoding)
    $writer.Write($text)
}
$imagePayload = New-BinaryPayload {
    param($writer)
    $writer.Write([UInt32] 2)
    $writer.Write([UInt32] 0)
    $writer.Write([UInt64] 4)
    $writer.Write([UInt32] 1)
    $writer.Write([UInt32] 1)
    $writer.Write([UInt32] 1)
    $writer.Write([UInt32] 1)
    $writer.Write([UInt32] 1)
    $writer.Write([UInt32] 4)
    $writer.Write([byte[]] @(0, 0, 0, 0))
}

Write-Seed protocol hello-auto (New-Frame 1 0 $helloPayload)
Write-Seed protocol ready (New-Frame 2 0 $readyPayload)
Write-Seed protocol resolve-extremes (New-Frame 3 ([UInt64]::MaxValue) $resolvePayload)
Write-Seed protocol result-status (New-Frame 4 42 $statusProtocolPayload)
Write-Seed protocol truncated-magic ([System.Text.Encoding]::ASCII.GetBytes('CPWK'))
Write-Seed protocol oversized-declaration (
    New-Frame 4 1 ([byte[]] @()) ([UInt32] (4 * 1024 * 1024 + 1))
)
$readyFrame = New-Frame 2 0 $readyPayload
$trailingFrame = [byte[]]::new($readyFrame.Length + 1)
[System.Array]::Copy($readyFrame, $trailingFrame, $readyFrame.Length)
$trailingFrame[$trailingFrame.Length - 1] = 0x7f
Write-Seed protocol trailing-byte $trailingFrame

Write-Seed payload status-unavailable $statusPayload
Write-Seed payload text-utf8 $textPayload
Write-Seed payload image-one-pixel $imagePayload
Write-Seed payload below-minimum ([byte[]] @(0, 0, 0, 0, 0, 0, 0))
Write-Seed payload unknown-kind (
    New-BinaryPayload {
        param($writer)
        $writer.Write([UInt32]::MaxValue)
        $writer.Write([UInt32] 0)
    }
)

Write-Seed content_sniff utf8 (
    New-BinaryPayload {
        param($writer)
        $writer.Write([byte] 0)
        $writer.Write([System.Text.Encoding]::UTF8.GetBytes('hello'))
    }
)
Write-Seed content_sniff truncated-utf8 ([byte[]] @(1, 0xe2, 0x82))
Write-Seed content_sniff utf16le-bom ([byte[]] @(0, 0xff, 0xfe, 0x41, 0, 0x42, 0))
Write-Seed content_sniff png ([byte[]] @(0, 0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a))
Write-Seed content_sniff webp (
    New-BinaryPayload {
        param($writer)
        $writer.Write([byte] 0)
        $writer.Write([System.Text.Encoding]::ASCII.GetBytes('RIFF0000WEBP'))
    }
)
Write-Seed content_sniff control-heavy ([byte[]] @(0, 1, 2, 3, 4, 5, 6, 7, 8))
Write-Seed content_sniff null-noise ([byte[]] @(0, 0, 0, 0, 0, 0, 0, 0, 0))

Write-Seed layout hd-to-preview (
    New-BinaryPayload {
        param($writer)
        $writer.Write([UInt32] 1920)
        $writer.Write([UInt32] 1080)
        $writer.Write([UInt32] 960)
        $writer.Write([UInt32] 720)
    }
)
Write-Seed layout zeros ([byte[]] (0..15 | ForEach-Object { 0 }))
Write-Seed layout extremes (
    New-BinaryPayload {
        param($writer)
        $writer.Write([UInt32]::MaxValue)
        $writer.Write([UInt32]::MaxValue)
        $writer.Write([UInt32] 1)
        $writer.Write([UInt32]::MaxValue)
    }
)
Write-Seed layout truncated ([byte[]] @(0x80, 0x07, 0))

$files = Get-ChildItem -LiteralPath $corpusRoot -File -Recurse | Sort-Object FullName
foreach ($file in $files) {
    $relative = $file.FullName.Substring($corpusRoot.Length).TrimStart(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = $sha256.ComputeHash([System.IO.File]::ReadAllBytes($file.FullName))
    }
    finally {
        $sha256.Dispose()
    }
    $hex = [System.BitConverter]::ToString($hash).Replace('-', '')
    "{0}`t{1}`t{2}" -f $relative, $file.Length, $hex
}
