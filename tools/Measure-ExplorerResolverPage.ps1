[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$FixturePath,

    [Parameter(Mandatory = $true)]
    [ValidateSet("windows10", "windows11")]
    [string]$Os,

    [Parameter(Mandatory = $true)]
    [ValidatePattern("^[0-9]{4,10}$")]
    [string]$Build,

    [Parameter(Mandatory = $true)]
    [ValidateSet("100", "125", "150", "175", "200")]
    [string]$Dpi,

    [Parameter(Mandatory = $true)]
    [ValidatePattern("^[a-z0-9][a-z0-9._-]{0,63}$")]
    [string]$Layout,

    [Parameter(Mandatory = $true)]
    [ValidatePattern("^[a-z0-9][a-z0-9._-]{0,63}$")]
    [string]$Scenario,

    [Parameter(Mandatory = $true)]
    [UInt64]$CaseIdStart,

    [Parameter(Mandatory = $true)]
    [ValidatePattern("^[a-z0-9][a-z0-9._-]{0,63}$")]
    [string]$SessionName,

    [ValidateSet("clickable", "row_three", "icon_five", "item_grid")]
    [string]$PointProfile = "clickable",

    [ValidateRange(1, 256)]
    [int]$MaxVisibleItems = 128,

    [ValidateRange(8, 128)]
    [int]$GridSpacingPixels = 16,

    [ValidateRange(1, 4)]
    [int]$GridRows = 2,

    [ValidateRange(16, 128)]
    [int]$GridEdgeInsetPixels = 32,

    [ValidateRange(1, 256)]
    [int]$MaxGridPointsPerItem = 128,

    [ValidateRange(1, 4096)]
    [int]$MaxCases = 4096,

    [ValidateRange(100, 10000)]
    [int]$TimeoutMilliseconds = 1250,

    [ValidateRange(0, 30)]
    [int]$ActivationDelaySeconds = 5,

    [ValidateRange(100, 2000)]
    [int]$SelectionSettleMilliseconds = 250,

    [string]$OutputDirectory,
    [string]$ScenarioMatrix,

    [switch]$SkipBuild,
    [switch]$ValidateOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
if ([string]::IsNullOrWhiteSpace($ScenarioMatrix)) {
    $ScenarioMatrix = Join-Path $repoRoot "corpus\scenarios.tsv"
}
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot "target\resolver-corpus\live\$SessionName"
}

$fixture = [System.IO.Path]::GetFullPath($FixturePath)
$matrixPath = [System.IO.Path]::GetFullPath($ScenarioMatrix)
$artifactDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
$statePath = Join-Path $artifactDirectory "state.json"
$artifactDirectoryValidated = $false
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$startedAt = [DateTime]::UtcNow

function Test-PathWithin {
    param(
        [string]$Candidate,
        [string]$Parent
    )

    $parentWithSeparator =
        $Parent.TrimEnd([System.IO.Path]::DirectorySeparatorChar) +
        [System.IO.Path]::DirectorySeparatorChar
    return $Candidate.StartsWith(
        $parentWithSeparator,
        [System.StringComparison]::OrdinalIgnoreCase
    )
}

trap {
    [Console]::Error.WriteLine($_.Exception.Message)
    if ($artifactDirectoryValidated -and
        -not [string]::IsNullOrWhiteSpace($artifactDirectory)) {
        [System.IO.Directory]::CreateDirectory($artifactDirectory) | Out-Null
        $failed = [ordered]@{
            status = "failed"
            session_name = $SessionName
            error = $_.Exception.Message
            script_stack_trace = $_.ScriptStackTrace
            started_at_utc = $startedAt.ToString("O")
            finished_at_utc = [DateTime]::UtcNow.ToString("O")
        } | ConvertTo-Json -Depth 6
        [System.IO.File]::WriteAllText($statePath, $failed, $utf8WithoutBom)
    }
    exit 1
}

if (-not [System.IO.Directory]::Exists($fixture)) {
    throw "The resolver fixture directory does not exist: $fixture"
}
if (-not [System.IO.File]::Exists($matrixPath)) {
    throw "The resolver scenario matrix does not exist: $matrixPath"
}

$protectedRoots = @(
    [System.IO.Path]::GetFullPath((Join-Path $repoRoot "corpus\results")),
    [System.IO.Path]::GetFullPath((Join-Path $repoRoot "qualification"))
)
foreach ($protectedRoot in $protectedRoots) {
    if ([System.StringComparer]::OrdinalIgnoreCase.Equals(
        $artifactDirectory,
        $protectedRoot
    ) -or (Test-PathWithin $artifactDirectory $protectedRoot)) {
        throw "Live collection must write to ignored review output, not accepted evidence paths."
    }
}

$matrixLines = [System.IO.File]::ReadAllLines($matrixPath, [System.Text.Encoding]::UTF8)
if ($matrixLines.Count -lt 2 -or
    $matrixLines[0] -cne "scenario`texpectation`tlayout`tnotes") {
    throw "The scenario matrix does not have the required header and rows."
}
$matrixMatches = @(
    for ($index = 1; $index -lt $matrixLines.Count; $index++) {
        $fields = $matrixLines[$index].Split([char]9)
        if ($fields.Count -ne 4) {
            throw "Scenario matrix line $($index + 1) does not have four fields."
        }
        if ($fields[0] -ceq $Scenario -and
            $fields[1] -ceq "resolve" -and
            $fields[2] -ceq $Layout) {
            $matrixLines[$index]
        }
    }
)
if ($matrixMatches.Count -ne 1) {
    throw "Scenario/layout must identify exactly one positive row in the scenario matrix."
}
if ($PointProfile -cne "item_grid" -and
    ($PSBoundParameters.ContainsKey("GridSpacingPixels") -or
     $PSBoundParameters.ContainsKey("GridRows") -or
     $PSBoundParameters.ContainsKey("GridEdgeInsetPixels") -or
     $PSBoundParameters.ContainsKey("MaxGridPointsPerItem"))) {
    throw "Grid parameters require PointProfile item_grid."
}

if ([System.IO.Directory]::Exists($artifactDirectory)) {
    $existingArtifacts = @(
        [System.IO.Directory]::EnumerateFileSystemEntries(
            $artifactDirectory,
            "*",
            [System.IO.SearchOption]::TopDirectoryOnly
        )
    )
    if ($existingArtifacts.Count -ne 0) {
        throw "The live-session output directory must be absent or empty."
    }
}
elseif ([System.IO.File]::Exists($artifactDirectory)) {
    throw "The live-session output path is an existing file."
}
$artifactDirectoryValidated = $true

Write-Host "Validated live Explorer resolver session: $SessionName"
Write-Host "Output remains review-only: $artifactDirectory"
if ($ValidateOnly) {
    return
}

[System.IO.Directory]::CreateDirectory($artifactDirectory) | Out-Null
$manifestPath = Join-Path $artifactDirectory "manifest.tsv"
$resultsPath = Join-Path $artifactDirectory "results.tsv"
$screenshotPath = Join-Path $artifactDirectory "explorer.png"
$probeTarget = Join-Path $repoRoot "target\resolver-corpus"
$probeExecutable = Join-Path $probeTarget "release\CursorPeek.exe"
$runner = Join-Path $PSScriptRoot "Test-ResolverCorpus.ps1"

if (-not $SkipBuild) {
    & cargo build `
        --manifest-path (Join-Path $repoRoot "Cargo.toml") `
        --locked `
        --release `
        --features resolver-corpus `
        --target-dir $probeTarget
    if ($LASTEXITCODE -ne 0) {
        throw "The resolver corpus probe build failed."
    }
}
if (-not [System.IO.File]::Exists($probeExecutable)) {
    throw "Resolver corpus probe executable not found: $probeExecutable"
}

if ($ActivationDelaySeconds -gt 0) {
    Write-Host "Prepare the exact Explorer fixture. Live labeling starts in $ActivationDelaySeconds seconds."
    Start-Sleep -Seconds $ActivationDelaySeconds
}

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.ComponentModel;
using System.Drawing;
using System.Runtime.InteropServices;

public static class CursorPeekLiveCorpusNative
{
    [StructLayout(LayoutKind.Sequential)]
    private struct MouseInput
    {
        public int Dx;
        public int Dy;
        public uint MouseData;
        public uint Flags;
        public uint Time;
        public IntPtr ExtraInfo;
    }

    [StructLayout(LayoutKind.Explicit)]
    private struct InputUnion
    {
        [FieldOffset(0)]
        public MouseInput Mouse;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Input
    {
        public uint Type;
        public InputUnion Union;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WindowRect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    private const uint InputMouse = 0;
    private const uint MouseLeftDown = 0x0002;
    private const uint MouseLeftUp = 0x0004;
    private const uint GetAncestorRoot = 2;

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint count, Input[] inputs, int size);

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern IntPtr WindowFromPhysicalPoint(Point point);

    [DllImport("user32.dll")]
    private static extern IntPtr GetAncestor(IntPtr window, uint flags);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr window);

    [DllImport("user32.dll")]
    private static extern bool IsIconic(IntPtr window);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint GetDpiForWindow(IntPtr window);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool GetWindowRect(IntPtr window, out WindowRect rectangle);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);

    public static void RequireUsableWindow(long expectedWindow)
    {
        IntPtr window = new IntPtr(expectedWindow);
        if (!IsWindowVisible(window) || IsIconic(window))
        {
            throw new InvalidOperationException("The exact Explorer frame is hidden or minimized.");
        }
    }

    public static void ClickAndConfirm(int x, int y, long expectedWindow)
    {
        InPhysicalContext(() =>
        {
            IntPtr expected = new IntPtr(expectedWindow);
            IntPtr target = GetAncestor(
                WindowFromPhysicalPoint(new Point(x, y)),
                GetAncestorRoot
            );
            if (target != expected)
            {
                throw new InvalidOperationException(
                    "The physical point belongs to a different top-level window."
                );
            }
            if (!SetCursorPos(x, y))
            {
                throw new Win32Exception();
            }
            Input[] inputs = { MouseEvent(MouseLeftDown), MouseEvent(MouseLeftUp) };
            uint sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(Input)));
            if (sent != inputs.Length)
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "SendInput did not insert the complete click."
                );
            }
            for (int attempt = 0; attempt < 20; attempt++)
            {
                System.Threading.Thread.Sleep(25);
                if (GetForegroundWindow() == expected)
                {
                    return;
                }
            }
            throw new InvalidOperationException(
                "The exact Explorer frame did not become foreground after the click."
            );
        });
    }

    public static uint DpiForWindow(long window)
    {
        uint dpi = 0;
        InPhysicalContext(() =>
        {
            dpi = GetDpiForWindow(new IntPtr(window));
            if (dpi == 0)
            {
                throw new Win32Exception();
            }
        });
        return dpi;
    }

    public static int[] BoundsForWindow(long window)
    {
        int[] bounds = null;
        InPhysicalContext(() =>
        {
            WindowRect rectangle;
            if (!GetWindowRect(new IntPtr(window), out rectangle))
            {
                throw new Win32Exception();
            }
            if (rectangle.Right <= rectangle.Left || rectangle.Bottom <= rectangle.Top)
            {
                throw new InvalidOperationException("The Explorer frame has invalid bounds.");
            }
            bounds = new[] {
                rectangle.Left,
                rectangle.Top,
                rectangle.Right,
                rectangle.Bottom
            };
        });
        return bounds;
    }

    private static void InPhysicalContext(Action action)
    {
        IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        if (previous == IntPtr.Zero)
        {
            throw new Win32Exception(
                Marshal.GetLastWin32Error(),
                "Could not enter the Per-Monitor-V2 thread context."
            );
        }
        try
        {
            action();
        }
        finally
        {
            SetThreadDpiAwarenessContext(previous);
        }
    }

    private static Input MouseEvent(uint flags)
    {
        Input input = new Input();
        input.Type = InputMouse;
        input.Union.Mouse.Flags = flags;
        return input;
    }
}
"@ -ReferencedAssemblies System.Drawing

$actualBuild = [Environment]::OSVersion.Version.Build
$actualOs = if ($actualBuild -ge 22000) { "windows11" } else { "windows10" }
if ($actualOs -cne $Os -or $actualBuild.ToString() -cne $Build) {
    throw "The live OS/build does not match the supplied labels."
}

$fixtureUrl = ([Uri]$fixture).AbsoluteUri
$shell = New-Object -ComObject Shell.Application
$windows = @(
    $shell.Windows() |
        Where-Object { [string]$_.LocationURL -ceq $fixtureUrl }
)
if ($windows.Count -ne 1) {
    throw "Expected exactly one Explorer frame at $fixtureUrl; found $($windows.Count)."
}
$window = $windows[0]
$explorerHwnd = [long]$window.HWND
[CursorPeekLiveCorpusNative]::RequireUsableWindow($explorerHwnd)

$expectedDpi = @{
    "100" = 96
    "125" = 120
    "150" = 144
    "175" = 168
    "200" = 192
}[$Dpi]
$actualDpi = [CursorPeekLiveCorpusNative]::DpiForWindow($explorerHwnd)
if ($actualDpi -ne $expectedDpi) {
    throw "Explorer HWND DPI $actualDpi does not match the $Dpi% label ($expectedDpi)."
}

$view = $window.Document
$viewMode = [int]$view.CurrentViewMode
$iconSize = try { [int]$view.IconSize } catch { $null }
$root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$explorerHwnd)
$dataItem = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::DataItem
)
$listItem = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::ListItem
)
$condition = [System.Windows.Automation.OrCondition]::new($dataItem, $listItem)

function Get-FullyVisibleFileElements {
    param(
        [System.Windows.Automation.AutomationElement]$Root,
        [System.Windows.Automation.Condition]$Condition
    )

    $elements = $Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        $Condition
    )
    if ($elements.Count -gt 512) {
        throw "The exact Explorer subtree exposes more than the 512-element search cap."
    }

    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $fullyVisible = [System.Collections.Generic.List[object]]::new()
    $clipped = 0
    for ($index = 0; $index -lt $elements.Count; $index++) {
        $element = $elements.Item($index)
        if ($element.Current.IsOffscreen) {
            continue
        }

        $container = $null
        $ancestor = $walker.GetParent($element)
        for ($depth = 0; $depth -lt 8 -and $null -ne $ancestor; $depth++) {
            if ($ancestor.Current.ControlType -eq
                [System.Windows.Automation.ControlType]::List) {
                $container = $ancestor
                break
            }
            $ancestor = $walker.GetParent($ancestor)
        }
        if ($null -eq $container) {
            throw "A visible file-item candidate has no bounded List ancestor."
        }

        $rectangle = $element.Current.BoundingRectangle
        $containerRectangle = $container.Current.BoundingRectangle
        if ($rectangle.Width -lt 2 -or
            $rectangle.Height -lt 2 -or
            $containerRectangle.Width -lt 2 -or
            $containerRectangle.Height -lt 2 -or
            $rectangle.Left -lt $containerRectangle.Left -or
            $rectangle.Top -lt $containerRectangle.Top -or
            $rectangle.Right -gt $containerRectangle.Right -or
            $rectangle.Bottom -gt $containerRectangle.Bottom) {
            $clipped++
            continue
        }
        $fullyVisible.Add([pscustomobject]@{
            Element = $element
            ContainerAutomationId = $container.Current.AutomationId
            ContainerRectangle = $containerRectangle
        })
    }

    return [pscustomobject]@{
        MatchingCount = $elements.Count
        ClippedCount = $clipped
        Elements = $fullyVisible.ToArray()
    }
}

function Get-PageFingerprint {
    param([object[]]$Entries)

    return @(
        foreach ($entry in $Entries) {
            $current = $entry.Element.Current
            $rectangle = $current.BoundingRectangle
            @(
                $current.Name,
                [Math]::Floor($rectangle.Left),
                [Math]::Floor($rectangle.Top),
                [Math]::Ceiling($rectangle.Right),
                [Math]::Ceiling($rectangle.Bottom),
                $entry.ContainerAutomationId
            ) -join "`t"
        }
    ) -join "`n"
}

$initialPage = Get-FullyVisibleFileElements $root $condition
$visible = @($initialPage.Elements)
if ($visible.Count -eq 0) {
    throw "The exact Explorer frame has no fully visible file-item candidates."
}
if ($visible.Count -gt $MaxVisibleItems) {
    throw "Visible item count $($visible.Count) exceeds the configured cap $MaxVisibleItems."
}
$initialFingerprint = Get-PageFingerprint $visible

function Add-UniquePoint {
    param(
        [System.Collections.Generic.List[object]]$Points,
        [System.Collections.Generic.HashSet[string]]$Keys,
        [int]$X,
        [int]$Y,
        [string]$Region,
        [System.Windows.Rect]$Rectangle
    )

    if ($X -lt [Math]::Floor($Rectangle.Left) -or
        $X -ge [Math]::Ceiling($Rectangle.Right) -or
        $Y -lt [Math]::Floor($Rectangle.Top) -or
        $Y -ge [Math]::Ceiling($Rectangle.Bottom)) {
        return
    }
    $key = "$X,$Y"
    if ($Keys.Add($key)) {
        $Points.Add([pscustomobject]@{
            X = $X
            Y = $Y
            Region = $Region
        })
    }
}

$cases = [System.Collections.Generic.List[object]]::new()
$skippedFolders = 0
$missingClickableItems = 0
$caseId = $CaseIdStart
$fixturePrefix =
    $fixture.TrimEnd([System.IO.Path]::DirectorySeparatorChar) +
    [System.IO.Path]::DirectorySeparatorChar

foreach ($entry in $visible) {
    $element = $entry.Element
    $rectangle = $element.Current.BoundingRectangle
    if ($null -eq $rectangle -or
        $null -eq $rectangle.PSObject.Properties["Width"] -or
        $null -eq $rectangle.PSObject.Properties["Height"]) {
        $entryType = if ($null -eq $entry) { "<null>" } else { $entry.GetType().FullName }
        $elementType = if ($null -eq $element) { "<null>" } else { $element.GetType().FullName }
        $rectangleType =
            if ($null -eq $rectangle) { "<null>" } else { $rectangle.GetType().FullName }
        throw "Unexpected UI Automation geometry shape (entry=$entryType; element=$elementType; rectangle=$rectangleType)."
    }
    if ($rectangle.Width -lt 2 -or $rectangle.Height -lt 2) {
        throw "A visible file-item candidate has invalid geometry."
    }
    $clickablePoint = [System.Windows.Point]::new(0, 0)
    $hasClickablePoint = $element.TryGetClickablePoint([ref]$clickablePoint)
    if (-not $hasClickablePoint -and $PointProfile -cne "item_grid") {
        throw "A visible file-item candidate has no physical clickable point."
    }
    if (-not $hasClickablePoint) {
        $missingClickableItems++
    }

    $left = [Convert]::ToInt32([Math]::Floor($rectangle.Left))
    $top = [Convert]::ToInt32([Math]::Floor($rectangle.Top))
    $right = [Convert]::ToInt32([Math]::Ceiling($rectangle.Right))
    $bottom = [Convert]::ToInt32([Math]::Ceiling($rectangle.Bottom))
    $centerX = $left + [int](($right - $left) / 2)
    $centerY = $top + [int](($bottom - $top) / 2)
    $insetX = [Math]::Max(2, [Math]::Min(12, [int](($right - $left) / 5)))
    $insetY = [Math]::Max(2, [Math]::Min(12, [int](($bottom - $top) / 5)))

    $points = [System.Collections.Generic.List[object]]::new()
    $pointKeys = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    if ($hasClickablePoint -and $PointProfile -cne "item_grid") {
        Add-UniquePoint $points $pointKeys `
            ([Convert]::ToInt32([Math]::Round($clickablePoint.X))) `
            ([Convert]::ToInt32([Math]::Round($clickablePoint.Y))) `
            "clickable" $rectangle
    }
    if ($PointProfile -cin @("row_three", "icon_five")) {
        Add-UniquePoint $points $pointKeys ($left + $insetX) $centerY "left" $rectangle
        Add-UniquePoint $points $pointKeys $centerX $centerY "center" $rectangle
        Add-UniquePoint $points $pointKeys ($right - $insetX - 1) $centerY "right" $rectangle
    }
    if ($PointProfile -ceq "icon_five") {
        Add-UniquePoint $points $pointKeys $centerX ($top + $insetY) "top" $rectangle
        Add-UniquePoint $points $pointKeys $centerX ($bottom - $insetY - 1) "bottom" $rectangle
    }
    if ($PointProfile -ceq "item_grid") {
        $gridLeft = $left + $GridEdgeInsetPixels
        $gridRight = $right - $GridEdgeInsetPixels - 1
        if ($gridLeft -gt $gridRight) {
            throw "A visible file-item candidate is too narrow for the configured grid edge inset."
        }
        for ($gridRow = 0; $gridRow -lt $GridRows; $gridRow++) {
            $gridY = $top + [Convert]::ToInt32(
                [Math]::Floor(
                    (($gridRow + 1) * ($bottom - $top)) / ($GridRows + 1)
                )
            )
            $gridColumn = 0
            for ($gridX = $gridLeft; $gridX -le $gridRight; $gridX += $GridSpacingPixels) {
                Add-UniquePoint $points $pointKeys $gridX $gridY `
                    "grid_r$($gridRow + 1)_c$($gridColumn + 1)" $rectangle
                $gridColumn++
            }
            if ((($gridRight - $gridLeft) % $GridSpacingPixels) -ne 0) {
                Add-UniquePoint $points $pointKeys $gridRight $gridY `
                    "grid_r$($gridRow + 1)_right" $rectangle
            }
        }
        if ($points.Count -gt $MaxGridPointsPerItem) {
            throw "A visible file-item candidate produced $($points.Count) points, exceeding MaxGridPointsPerItem $MaxGridPointsPerItem."
        }
    }
    if (($cases.Count + $points.Count) -gt $MaxCases) {
        throw "The live page would exceed the hard MaxCases limit of $MaxCases."
    }

    $elementPath = $null
    foreach ($point in $points) {
        try {
            [CursorPeekLiveCorpusNative]::ClickAndConfirm(
                $point.X,
                $point.Y,
                $explorerHwnd
            )
        }
        catch {
            throw "Point $($point.Region) at ($($point.X),$($point.Y)) failed: $($_.Exception.Message)"
        }
        Start-Sleep -Milliseconds $SelectionSettleMilliseconds
        if ([long]$window.HWND -ne $explorerHwnd -or
            [string]$window.LocationURL -cne $fixtureUrl) {
            throw "Explorer navigated or the exact frame identity changed during labeling."
        }

        $selected = $window.Document.SelectedItems()
        if ($selected.Count -ne 1) {
            throw "A file-item point did not produce exactly one selected Explorer item."
        }
        $selectedItem = $selected.Item(0)
        if ([bool]$selectedItem.IsFolder) {
            $skippedFolders++
            $elementPath = $null
            break
        }
        if (-not [bool]$selectedItem.IsFileSystem) {
            throw "The selected item is not a filesystem item."
        }
        $selectedPath = [System.IO.Path]::GetFullPath([string]$selectedItem.Path)
        if (-not $selectedPath.StartsWith(
            $fixturePrefix,
            [System.StringComparison]::OrdinalIgnoreCase
        ) -or -not [System.IO.File]::Exists($selectedPath)) {
            throw "The selected path is not an existing file inside the exact fixture."
        }
        if ($null -eq $elementPath) {
            $elementPath = $selectedPath
        }
        elseif (-not [System.StringComparer]::OrdinalIgnoreCase.Equals(
            $elementPath,
            $selectedPath
        )) {
            throw "Different points in one UIA item selected different filesystem paths."
        }

        $cases.Add([pscustomobject]@{
            CaseId = $caseId
            X = $point.X
            Y = $point.Y
            ExpectedPath = $selectedPath
            Region = $point.Region
        })
        if ($caseId -eq [UInt64]::MaxValue) {
            throw "Case ID range overflowed."
        }
        $caseId++
    }
}
if ($cases.Count -eq 0) {
    throw "Live labeling produced no positive file cases."
}

$finalPage = Get-FullyVisibleFileElements $root $condition
$finalVisible = @($finalPage.Elements)
$finalFingerprint = Get-PageFingerprint $finalVisible
if ($initialFingerprint -cne $finalFingerprint) {
    throw "The visible Explorer page scrolled, reordered, or changed during labeling."
}

$manifestLines = [System.Collections.Generic.List[string]]::new()
$manifestLines.Add(
    "case_id`tos`tbuild`tdpi`tlayout`tscenario`tx`ty`texpectation`texpected_path"
)
foreach ($case in $cases) {
    $manifestLines.Add(@(
        $case.CaseId,
        $Os,
        $Build,
        $Dpi,
        $Layout,
        $Scenario,
        $case.X,
        $case.Y,
        "resolve",
        $case.ExpectedPath
    ) -join "`t")
}
[System.IO.File]::WriteAllLines($manifestPath, $manifestLines, $utf8WithoutBom)

$bounds = [CursorPeekLiveCorpusNative]::BoundsForWindow($explorerHwnd)
$width = $bounds[2] - $bounds[0]
$height = $bounds[3] - $bounds[1]
$bitmap = [System.Drawing.Bitmap]::new($width, $height)
try {
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen(
            $bounds[0],
            $bounds[1],
            0,
            0,
            [System.Drawing.Size]::new($width, $height),
            [System.Drawing.CopyPixelOperation]::SourceCopy
        )
    }
    finally {
        $graphics.Dispose()
    }
    $bitmap.Save($screenshotPath, [System.Drawing.Imaging.ImageFormat]::Png)
}
finally {
    $bitmap.Dispose()
}

[CursorPeekLiveCorpusNative]::ClickAndConfirm(
    $cases[0].X,
    $cases[0].Y,
    $explorerHwnd
)
& $runner `
    -Manifest $manifestPath `
    -Results $resultsPath `
    -TimeoutMilliseconds $TimeoutMilliseconds `
    -ActivationDelaySeconds 0 `
    -SkipBuild

$resultLines = [System.IO.File]::ReadAllLines($resultsPath, [System.Text.Encoding]::UTF8)
if ($resultLines.Count -ne ($cases.Count + 1)) {
    throw "The raw result row count does not match the live manifest."
}
for ($index = 1; $index -lt $resultLines.Count; $index++) {
    $fields = $resultLines[$index].Split([char]9)
    if ($fields.Count -ne 17 -or
        $fields[10] -cne "resolved" -or
        $fields[16] -cne "correct_positive" -or
        -not [System.StringComparer]::OrdinalIgnoreCase.Equals(
            $fields[9],
            $fields[11]
        )) {
        throw "The raw result contains a miss, wrong path, failure, or malformed row."
    }
}

$probeHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $probeExecutable).Hash
$manifestHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $manifestPath).Hash
$resultsHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $resultsPath).Hash
$screenshotHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $screenshotPath).Hash
$state = [ordered]@{
    status = "passed"
    session_name = $SessionName
    fixture_path = $fixture
    fixture_url = $fixtureUrl
    os = $Os
    build = $Build
    dpi_percent = $Dpi
    dpi = $actualDpi
    layout = $Layout
    scenario = $Scenario
    point_profile = $PointProfile
    selection_settle_ms = $SelectionSettleMilliseconds
    max_cases = $MaxCases
    grid = if ($PointProfile -ceq "item_grid") {
        [ordered]@{
            spacing_pixels = $GridSpacingPixels
            rows = $GridRows
            edge_inset_pixels = $GridEdgeInsetPixels
            max_points_per_item = $MaxGridPointsPerItem
        }
    }
    else {
        $null
    }
    explorer_hwnd = $explorerHwnd
    explorer_bounds = $bounds
    shell_view_mode = $viewMode
    shell_icon_size = $iconSize
    interactive_session_id = (Get-Process -Id $PID).SessionId
    matching_uia_elements = $initialPage.MatchingCount
    visible_uia_elements = $visible.Count
    clipped_uia_elements = $initialPage.ClippedCount
    skipped_folders = $skippedFolders
    missing_clickable_items = $missingClickableItems
    labeled_rows = $cases.Count
    case_id_first = $cases[0].CaseId
    case_id_last = $cases[$cases.Count - 1].CaseId
    probe = [ordered]@{
        file = [System.IO.Path]::GetFileName($probeExecutable)
        sha256 = $probeHash
    }
    artifacts = [ordered]@{
        manifest = [ordered]@{
            file = [System.IO.Path]::GetFileName($manifestPath)
            sha256 = $manifestHash
        }
        results = [ordered]@{
            file = [System.IO.Path]::GetFileName($resultsPath)
            sha256 = $resultsHash
        }
        screenshot = [ordered]@{
            file = [System.IO.Path]::GetFileName($screenshotPath)
            sha256 = $screenshotHash
        }
    }
    started_at_utc = $startedAt.ToString("O")
    finished_at_utc = [DateTime]::UtcNow.ToString("O")
}
[System.IO.File]::WriteAllText(
    $statePath,
    ($state | ConvertTo-Json -Depth 8),
    $utf8WithoutBom
)

Write-Host "Live resolver page captured for review: $artifactDirectory"
Write-Host "Rows=$($cases.Count); manifest=$manifestHash; results=$resultsHash"
