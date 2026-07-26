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
$headersOutput = & $dumpbin.FullName /nologo /headers $resolvedExecutable
if ($LASTEXITCODE -ne 0) {
    throw "dumpbin.exe header inspection failed with exit code $LASTEXITCODE."
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
    'ADVAPI32.DLL'
    'API-MS-WIN-CORE-SYNCH-L1-2-0.DLL'
    'API-MS-WIN-CORE-WINRT-L1-1-0.DLL'
    'BCRYPT.DLL'
    'BCRYPTPRIMITIVES.DLL'
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

$subsystemLine = $headersOutput |
    Where-Object { $_ -match '^\s+([0-9A-Fa-f]+)\s+subsystem \((.+)\)\s*$' } |
    Select-Object -First 1
if ($null -eq $subsystemLine -or $subsystemLine -notmatch '^\s+([0-9A-Fa-f]+)\s+subsystem \((.+)\)\s*$') {
    throw 'Could not read the PE subsystem.'
}
$subsystem = [Convert]::ToUInt32($Matches[1], 16)
$subsystemName = $Matches[2]
if ($subsystem -ne 2 -or $subsystemName -cne 'Windows GUI') {
    throw "Expected PE subsystem 2 (Windows GUI); found $subsystem ($subsystemName)."
}

$resourceLine = $headersOutput |
    Where-Object {
        $_ -match '^\s+([0-9A-Fa-f]+)\s+\[\s*([0-9A-Fa-f]+)\]\s+RVA \[size\] of Resource Directory\s*$'
    } |
    Select-Object -First 1
if (
    $null -eq $resourceLine -or
    $resourceLine -notmatch '^\s+([0-9A-Fa-f]+)\s+\[\s*([0-9A-Fa-f]+)\]\s+RVA \[size\] of Resource Directory\s*$'
) {
    throw 'Could not read the PE resource directory.'
}
$resourceRva = [Convert]::ToUInt32($Matches[1], 16)
$resourceBytes = [Convert]::ToUInt32($Matches[2], 16)
if ($resourceRva -eq 0 -or $resourceBytes -eq 0) {
    throw 'The PE resource directory is empty.'
}

$metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) {
    throw 'Could not read Cargo package metadata.'
}
$package = $metadata.packages |
    Where-Object { $_.name -ceq 'windows-cursorpeek' } |
    Select-Object -First 1
if ($null -eq $package) {
    throw 'Cargo metadata did not contain the windows-cursorpeek package.'
}
$expectedVersion = [string] $package.version
$versionInfo = $artifact.VersionInfo
$expectedVersionFields = [ordered] @{
    CompanyName = 'CursorPeek contributors'
    FileDescription = 'File Explorer hover previews'
    FileVersion = "$expectedVersion.0"
    InternalName = 'CursorPeek'
    OriginalFilename = 'CursorPeek.exe'
    ProductName = 'CursorPeek'
    ProductVersion = $expectedVersion
}
foreach ($field in $expectedVersionFields.GetEnumerator()) {
    if ([string] $versionInfo.($field.Key) -cne [string] $field.Value) {
        throw "Version field $($field.Key) is '$($versionInfo.($field.Key))'; expected '$($field.Value)'."
    }
}

Add-Type -AssemblyName System.Drawing
$extractedIcon = [System.Drawing.Icon]::ExtractAssociatedIcon($resolvedExecutable)
if ($null -eq $extractedIcon) {
    throw 'The release artifact does not expose an associated application icon.'
}
try {
    $iconWidth = $extractedIcon.Width
    $iconHeight = $extractedIcon.Height
    if ($iconWidth -le 0 -or $iconHeight -le 0) {
        throw 'The extracted application icon has invalid dimensions.'
    }
}
finally {
    $extractedIcon.Dispose()
}

$logoPath = Join-Path $PSScriptRoot '..\assets\windows\CursorPeek.png'
$expectedLogoHash = '096FDCF9A0CEE5DDF83728593FF47AA7B600047317A8E19D21FD730F88BB5AF8'
$logoHash = (Get-FileHash -LiteralPath $logoPath -Algorithm SHA256).Hash
if ($logoHash -cne $expectedLogoHash) {
    throw "The canonical logo hash is $logoHash; expected $expectedLogoHash."
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
    pe = [ordered] @{
        subsystem = $subsystem
        subsystem_name = $subsystemName
        resource_rva = $resourceRva
        resource_bytes = $resourceBytes
    }
    version_info = $expectedVersionFields
    application_icon = [ordered] @{
        extracted_width = $iconWidth
        extracted_height = $iconHeight
        canonical_logo_sha256 = $logoHash
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
| PE subsystem | ``$subsystem ($subsystemName)`` |
| Resources | $resourceBytes bytes; application icon $($iconWidth)x$iconHeight |
| Direct imports | $($imports.Count) approved system DLL families |
| Rust | ``$rustVersion`` |
"@
if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_STEP_SUMMARY)) {
    [System.IO.File]::AppendAllText($env:GITHUB_STEP_SUMMARY, "$summary`n", $utf8)
}

Write-Output $summary
