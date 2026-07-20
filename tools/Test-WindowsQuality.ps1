[CmdletBinding()]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $Executable = 'target/release/CursorPeek.exe',

    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string] $OutputPath = 'target/windows-quality/release-evidence.json',

    [Parameter()]
    [ValidateRange(1, [long]::MaxValue)]
    [long] $MaximumBytes = 3MB
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
$artifact = Get-Item -LiteralPath $resolvedExecutable
if ($artifact.Length -gt $MaximumBytes) {
    throw "Release artifact is $($artifact.Length) bytes; the limit is $MaximumBytes bytes."
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
    throw "Could not locate vswhere.exe at '$vswhere'."
}

$installation = & $vswhere `
    -latest `
    -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($installation)) {
    throw 'Could not locate an MSVC installation with the x64 C++ tools.'
}

$dumpbin = Get-ChildItem `
    -LiteralPath (Join-Path $installation 'VC\Tools\MSVC') `
    -Recurse `
    -Filter dumpbin.exe `
    -File |
    Where-Object FullName -Like '*\bin\Hostx64\x64\dumpbin.exe' |
    Sort-Object LastWriteTimeUtc -Descending |
    Select-Object -First 1
if ($null -eq $dumpbin) {
    throw 'Could not locate the x64 dumpbin.exe.'
}

$dumpbinOutput = & $dumpbin.FullName /nologo /dependents $resolvedExecutable
if ($LASTEXITCODE -ne 0) {
    throw "dumpbin.exe failed with exit code $LASTEXITCODE."
}

$imports = @(
    $dumpbinOutput |
        ForEach-Object {
            if ($_ -match '^\s+([A-Za-z0-9_.-]+\.dll)\s*$') {
                $Matches[1].ToUpperInvariant()
            }
        } |
        Sort-Object -Unique
)
if ($imports.Count -eq 0) {
    throw 'The release artifact did not expose any direct DLL imports.'
}

$approvedSystemImports = @(
    'API-MS-WIN-CORE-SYNCH-L1-2-0.DLL'
    'BCRYPT.DLL'
    'COMBASE.DLL'
    'D2D1.DLL'
    'DWRITE.DLL'
    'KERNEL32.DLL'
    'NTDLL.DLL'
    'OLE32.DLL'
    'OLEAUT32.DLL'
    'SHELL32.DLL'
    'USER32.DLL'
)
$unexpectedImports = @($imports | Where-Object { $_ -notin $approvedSystemImports })
if ($unexpectedImports.Count -ne 0) {
    throw "Unexpected direct DLL imports: $($unexpectedImports -join ', ')."
}

$hash = (Get-FileHash -LiteralPath $resolvedExecutable -Algorithm SHA256).Hash
$rustVersion = (rustc --version).Trim()
$cargoVersion = (cargo --version).Trim()
if ($LASTEXITCODE -ne 0) {
    throw 'Could not read the pinned Rust tool versions.'
}

$sourceRevision = if ([string]::IsNullOrWhiteSpace($env:GITHUB_SHA)) {
    (git rev-parse HEAD).Trim()
} else {
    $env:GITHUB_SHA
}
if ($LASTEXITCODE -ne 0) {
    throw 'Could not determine the source revision.'
}

$evidence = [ordered] @{
    schema_version = 1
    source_revision = $sourceRevision
    target = 'x86_64-pc-windows-msvc'
    rust = $rustVersion
    cargo = $cargoVersion
    artifact = [ordered] @{
        name = $artifact.Name
        bytes = $artifact.Length
        maximum_bytes = $MaximumBytes
        sha256 = $hash
    }
    direct_imports = $imports
}

$outputDirectory = Split-Path -Parent $OutputPath
if (-not [string]::IsNullOrWhiteSpace($outputDirectory)) {
    New-Item -ItemType Directory -Force $outputDirectory | Out-Null
}
$json = $evidence | ConvertTo-Json -Depth 4
$utf8 = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText(
    [System.IO.Path]::GetFullPath($OutputPath),
    "$json`n",
    $utf8
)

$summary = @"
### Windows release evidence

| Field | Value |
|---|---|
| Artifact | ``$($artifact.Name)`` |
| Size | $($artifact.Length) bytes |
| SHA-256 | ``$hash`` |
| Direct imports | $($imports.Count) approved system DLL families |
| Rust | ``$rustVersion`` |
"@
if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_STEP_SUMMARY)) {
    [System.IO.File]::AppendAllText($env:GITHUB_STEP_SUMMARY, "$summary`n", $utf8)
}

Write-Output $summary
