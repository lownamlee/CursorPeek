[CmdletBinding()]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $InputPath = (Join-Path $PSScriptRoot '..\assets\windows\CursorPeek.png'),

    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $OutputPath = (Join-Path $PSScriptRoot '..\assets\windows\CursorPeek.ico')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-Sha256Hex {
    param([Parameter(Mandatory = $true)][string]$LiteralPath)

    $stream = [System.IO.File]::OpenRead($LiteralPath)
    try {
        $algorithm = [System.Security.Cryptography.SHA256]::Create()
        try {
            return [System.BitConverter]::ToString(
                $algorithm.ComputeHash($stream)
            ).Replace("-", "")
        }
        finally {
            $algorithm.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

$expectedSourceHash = '096FDCF9A0CEE5DDF83728593FF47AA7B600047317A8E19D21FD730F88BB5AF8'
$iconSizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)

$resolvedInput = (Resolve-Path -LiteralPath $InputPath).Path
$sourceHash = Get-Sha256Hex -LiteralPath $resolvedInput
if ($sourceHash -cne $expectedSourceHash) {
    throw "The logo hash is $sourceHash; expected the approved source $expectedSourceHash."
}

Add-Type -AssemblyName System.Drawing

$source = [System.Drawing.Image]::FromFile($resolvedInput)
$frames = [System.Collections.Generic.List[byte[]]]::new()
try {
    if ($source.Width -ne 100 -or $source.Height -ne 98) {
        throw "The approved logo must be 100x98 pixels; found $($source.Width)x$($source.Height)."
    }
    if (($source.Flags -band [int][System.Drawing.Imaging.ImageFlags]::HasAlpha) -eq 0) {
        throw 'The approved logo must retain an alpha channel.'
    }

    foreach ($size in $iconSizes) {
        $bitmap = [System.Drawing.Bitmap]::new(
            $size,
            $size,
            [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
        )
        try {
            $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
            try {
                $graphics.Clear([System.Drawing.Color]::Transparent)
                $graphics.CompositingMode =
                    [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
                $graphics.CompositingQuality =
                    [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
                $graphics.InterpolationMode =
                    [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                $graphics.PixelOffsetMode =
                    [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
                $graphics.SmoothingMode =
                    [System.Drawing.Drawing2D.SmoothingMode]::HighQuality

                $scale = [Math]::Min(
                    [double] $size / [double] $source.Width,
                    [double] $size / [double] $source.Height
                )
                $width = [Math]::Max(1, [int][Math]::Round($source.Width * $scale))
                $height = [Math]::Max(1, [int][Math]::Round($source.Height * $scale))
                $x = [int][Math]::Floor(($size - $width) / 2)
                $y = [int][Math]::Floor(($size - $height) / 2)
                $destination = [System.Drawing.Rectangle]::new($x, $y, $width, $height)

                $graphics.DrawImage(
                    $source,
                    $destination,
                    0,
                    0,
                    $source.Width,
                    $source.Height,
                    [System.Drawing.GraphicsUnit]::Pixel
                )
            }
            finally {
                $graphics.Dispose()
            }

            $stream = [System.IO.MemoryStream]::new()
            try {
                $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
                $frames.Add($stream.ToArray())
            }
            finally {
                $stream.Dispose()
            }
        }
        finally {
            $bitmap.Dispose()
        }
    }
}
finally {
    $source.Dispose()
}

$resolvedOutput = [System.IO.Path]::GetFullPath($OutputPath)
$outputDirectory = Split-Path -Parent $resolvedOutput
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
$temporary = Join-Path $outputDirectory ".CursorPeek.$([Guid]::NewGuid().ToString('N')).ico.tmp"

try {
    $file = [System.IO.File]::Open(
        $temporary,
        [System.IO.FileMode]::CreateNew,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::None
    )
    try {
        $writer = [System.IO.BinaryWriter]::new($file)
        try {
            $writer.Write([uint16] 0)
            $writer.Write([uint16] 1)
            $writer.Write([uint16] $iconSizes.Count)

            $offset = [uint32] (6 + (16 * $iconSizes.Count))
            for ($index = 0; $index -lt $iconSizes.Count; $index++) {
                $size = $iconSizes[$index]
                $frame = $frames[$index]
                $encodedSize = if ($size -eq 256) { [byte] 0 } else { [byte] $size }

                $writer.Write($encodedSize)
                $writer.Write($encodedSize)
                $writer.Write([byte] 0)
                $writer.Write([byte] 0)
                $writer.Write([uint16] 1)
                $writer.Write([uint16] 32)
                $writer.Write([uint32] $frame.Length)
                $writer.Write($offset)
                $offset += [uint32] $frame.Length
            }

            foreach ($frame in $frames) {
                $writer.Write($frame)
            }
            $writer.Flush()
            $file.Flush($true)
        }
        finally {
            $writer.Dispose()
        }
    }
    finally {
        $file.Dispose()
    }

    Move-Item -LiteralPath $temporary -Destination $resolvedOutput -Force
}
finally {
    if (Test-Path -LiteralPath $temporary) {
        Remove-Item -LiteralPath $temporary -Force
    }
}

[pscustomobject] @{
    Source = $resolvedInput
    SourceSha256 = $sourceHash
    Output = $resolvedOutput
    OutputSha256 = Get-Sha256Hex -LiteralPath $resolvedOutput
    Sizes = $iconSizes -join ','
}
