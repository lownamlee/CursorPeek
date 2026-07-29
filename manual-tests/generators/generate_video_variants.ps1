[CmdletBinding()]
param(
    [Parameter()]
    [string] $InputPath = (
        Join-Path $PSScriptRoot '..\videos\sample.mp4'
    ),

    [Parameter()]
    [string] $OutputDirectory = (
        Join-Path $PSScriptRoot '..\videos'
    )
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$ffmpeg = (Get-Command ffmpeg.exe -ErrorAction Stop).Source
$ffprobe = (Get-Command ffprobe.exe -ErrorAction Stop).Source
$source = (Resolve-Path -LiteralPath $InputPath).Path
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$output = (Resolve-Path -LiteralPath $OutputDirectory).Path

function Invoke-Ffmpeg {
    param(
        [Parameter(Mandatory)]
        [string[]] $Arguments,

        [Parameter(Mandatory)]
        [string] $ExpectedOutput
    )

    & $ffmpeg -hide_banner -loglevel error -y -i $source @Arguments $ExpectedOutput
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $ExpectedOutput)) {
        throw "FFmpeg did not create '$ExpectedOutput'."
    }
}

foreach ($extension in @('m4v', 'mp4v')) {
    Invoke-Ffmpeg `
        -Arguments @('-map', '0:v:0', '-an', '-c:v', 'copy', '-movflags', '+faststart', '-f', 'mp4') `
        -ExpectedOutput (Join-Path $output "sample.$extension")
}

Invoke-Ffmpeg `
    -Arguments @('-map', '0:v:0', '-an', '-c:v', 'copy', '-f', 'mov') `
    -ExpectedOutput (Join-Path $output 'sample.mov')

foreach ($variant in @(
    @{ Extension = '3gp'; Format = '3gp' },
    @{ Extension = '3gpp'; Format = '3gp' },
    @{ Extension = '3g2'; Format = '3g2' },
    @{ Extension = '3gp2'; Format = '3g2' }
)) {
    Invoke-Ffmpeg `
        -Arguments @(
            '-map', '0:v:0',
            '-an',
            '-vf', 'scale=480:-2',
            '-c:v', 'libx264',
            '-preset', 'medium',
            '-profile:v', 'baseline',
            '-level:v', '3.0',
            '-crf', '24',
            '-pix_fmt', 'yuv420p',
            '-movflags', '+faststart',
            '-f', $variant.Format
        ) `
        -ExpectedOutput (Join-Path $output "sample.$($variant.Extension)")
}

Invoke-Ffmpeg `
    -Arguments @(
        '-map', '0:v:0',
        '-an',
        '-c:v', 'mpeg4',
        '-q:v', '5',
        '-pix_fmt', 'yuv420p',
        '-f', 'avi'
    ) `
    -ExpectedOutput (Join-Path $output 'sample.avi')

foreach ($extension in @('asf', 'wmv')) {
    Invoke-Ffmpeg `
        -Arguments @(
            '-map', '0:v:0',
            '-an',
            '-c:v', 'wmv2',
            '-b:v', '700k',
            '-pix_fmt', 'yuv420p',
            '-f', 'asf'
        ) `
        -ExpectedOutput (Join-Path $output "sample.$extension")
}

$videos = @(Get-ChildItem -LiteralPath $output -File | Sort-Object Extension)
foreach ($video in $videos) {
    $probe = & $ffprobe `
        -v error `
        -select_streams v:0 `
        -show_entries 'format=format_name,duration:stream=codec_name,width,height' `
        -of json `
        $video.FullName |
        ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or @($probe.streams).Count -ne 1) {
        throw "FFprobe could not validate '$($video.FullName)'."
    }
    [pscustomobject]@{
        File = $video.Name
        Bytes = $video.Length
        Container = [string] $probe.format.format_name
        Codec = [string] $probe.streams[0].codec_name
        Width = [int] $probe.streams[0].width
        Height = [int] $probe.streams[0].height
        DurationSeconds = [Math]::Round([double] $probe.format.duration, 3)
    }
}
