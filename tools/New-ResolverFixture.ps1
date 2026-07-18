[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Destination,

    [ValidateRange(8, 2000)]
    [int]$FileCount = 256,

    [string]$Inventory,

    [switch]$ValidateOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$destinationPath = [System.IO.Path]::GetFullPath($Destination)
$destinationRoot = [System.IO.Path]::GetPathRoot($destinationPath)
if ([System.StringComparer]::OrdinalIgnoreCase.Equals(
    $destinationPath.TrimEnd([System.IO.Path]::DirectorySeparatorChar),
    $destinationRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar)
)) {
    throw "The resolver fixture destination cannot be a filesystem root."
}
if ([string]::IsNullOrWhiteSpace($Inventory)) {
    $Inventory = "$($destinationPath.TrimEnd([System.IO.Path]::DirectorySeparatorChar)).inventory.tsv"
}
$inventoryPath = [System.IO.Path]::GetFullPath($Inventory)
$destinationPrefix =
    $destinationPath.TrimEnd([System.IO.Path]::DirectorySeparatorChar) +
    [System.IO.Path]::DirectorySeparatorChar
if ([System.StringComparer]::OrdinalIgnoreCase.Equals($destinationPath, $inventoryPath) -or
    $inventoryPath.StartsWith(
        $destinationPrefix,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
    throw "The fixture inventory must remain outside the fixture directory."
}

if ([System.IO.Directory]::Exists($destinationPath)) {
    $existing = @(
        [System.IO.Directory]::EnumerateFileSystemEntries(
            $destinationPath,
            "*",
            [System.IO.SearchOption]::TopDirectoryOnly
        )
    )
    if ($existing.Count -ne 0) {
        throw "The resolver fixture destination must be absent or empty."
    }
}
elseif ([System.IO.File]::Exists($destinationPath)) {
    throw "The resolver fixture destination is an existing file."
}
if ([System.IO.File]::Exists($inventoryPath) -or
    [System.IO.Directory]::Exists($inventoryPath)) {
    throw "The fixture inventory path already exists."
}

Write-Host "Validated resolver fixture destination: $destinationPath"
Write-Host "Fixture files: $FileCount"
Write-Host "Inventory: $inventoryPath"
if ($ValidateOnly) {
    return
}

[System.IO.Directory]::CreateDirectory($destinationPath) | Out-Null
$inventoryParent = [System.IO.Path]::GetDirectoryName($inventoryPath)
if (-not [string]::IsNullOrEmpty($inventoryParent)) {
    [System.IO.Directory]::CreateDirectory($inventoryParent) | Out-Null
}

$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$inventoryLines = [System.Collections.Generic.List[string]]::new()
$inventoryLines.Add("relative_path`tsha256`tbytes")
for ($index = 1; $index -le $FileCount; $index++) {
    $name = "cursorpeek-item-{0:D4}.txt" -f $index
    $path = Join-Path $destinationPath $name
    $content = "CursorPeek resolver fixture {0:D4}`r`n" -f $index
    [System.IO.File]::WriteAllText($path, $content, $utf8WithoutBom)
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash
    $length = (Get-Item -LiteralPath $path).Length
    $inventoryLines.Add("$name`t$hash`t$length")
}

[System.IO.File]::WriteAllLines($inventoryPath, $inventoryLines, $utf8WithoutBom)
Write-Host "Created $FileCount deterministic resolver fixture files."
