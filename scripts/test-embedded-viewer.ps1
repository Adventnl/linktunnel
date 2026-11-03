<#
Verify a real remote stream in a hidden native child window. Sends no remote input,
does not play audio, and never changes the installed host identity or configuration.
Use a paired device ID or a device name without spaces.
#>
param(
    [Parameter(Mandatory)][ValidatePattern('^[A-Za-z0-9_-]+$')][string]$Device,
    [string]$Viewer = "$PSScriptRoot\..\target\debug\nexus.exe"
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EmbeddedViewerTest {
  public delegate bool Visitor(IntPtr hwnd, IntPtr data);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr hwnd, Visitor callback, IntPtr data);
  [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr hwnd, int index);
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
}
'@
$form = [Windows.Forms.Form]::new()
$form.ClientSize = [Drawing.Size]::new(1000, 700)
$parentHandle = $form.Handle # Create a hidden parent; no user-visible window or focus change.
$process = [Diagnostics.Process]::new()
$process.StartInfo.FileName = (Resolve-Path -LiteralPath $Viewer).Path
$process.StartInfo.Arguments = "--system remote view $Device --view-only --no-clipboard"
$process.StartInfo.UseShellExecute = $false
$process.StartInfo.CreateNoWindow = $true
$process.StartInfo.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
$process.StartInfo.RedirectStandardOutput = $true
$process.StartInfo.RedirectStandardError = $true
$process.StartInfo.RedirectStandardInput = $true
$process.StartInfo.EnvironmentVariables['NEXUS_VIEWER_PARENT'] = $parentHandle.ToInt64().ToString()
$process.StartInfo.EnvironmentVariables['NEXUS_VIEWER_STATUS'] = '1'
$process.StartInfo.EnvironmentVariables['NEXUS_VIEWER_VIEWPORT'] = '{"x":20,"y":80,"width":800,"height":500,"visible":true,"scale":"fit"}'
try {
    if (-not $process.Start()) { throw 'Viewer did not start.' }
    $errors = $process.StandardError.ReadToEndAsync()
    $line = $process.StandardOutput.ReadLineAsync()
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    $ready = $false
    while ([DateTime]::UtcNow -lt $deadline -and -not $process.HasExited) {
        [Windows.Forms.Application]::DoEvents()
        if ($line.IsCompleted) {
            $value = $line.Result
            if ($null -eq $value) { break }
            try { $status = $value | ConvertFrom-Json } catch { $status = $null }
            if ($status.nexusViewer -eq 'ready') { $ready = $true; break }
            $line = $process.StandardOutput.ReadLineAsync()
        }
        Start-Sleep -Milliseconds 20
    }
    if (-not $ready) { throw 'No decoded remote picture arrived within 45 seconds.' }
    $children = [Collections.Generic.List[IntPtr]]::new()
    $visit = [EmbeddedViewerTest+Visitor]{ param($hwnd, $data) $children.Add($hwnd); return $true }
    $null = [EmbeddedViewerTest]::EnumChildWindows($parentHandle, $visit, [IntPtr]::Zero)
    $child = $children | Where-Object { [EmbeddedViewerTest]::GetParent($_) -eq $parentHandle } | Select-Object -First 1
    if (-not $child) { throw 'Viewer is not a child of the workspace.' }
    if (([EmbeddedViewerTest]::GetWindowLong($child, -16) -band 0x40000000) -eq 0) { throw 'Viewer lacks WS_CHILD.' }
    $process.StandardInput.WriteLine('{"x":30,"y":90,"width":777,"height":444,"visible":true,"scale":"fill"}')
    $process.StandardInput.Flush()
    $deadline = [DateTime]::UtcNow.AddSeconds(3)
    do {
        [Windows.Forms.Application]::DoEvents()
        Start-Sleep -Milliseconds 20
        $rect = [EmbeddedViewerTest+Rect]::new()
        $null = [EmbeddedViewerTest]::GetClientRect($child, [ref]$rect)
    } while (($rect.Right -ne 777 -or $rect.Bottom -ne 444) -and [DateTime]::UtcNow -lt $deadline)
    if ($rect.Right -ne 777 -or $rect.Bottom -ne 444) { throw 'Native viewport did not follow the workspace resize.' }
    $process.StandardInput.WriteLine('{"x":30,"y":90,"width":777,"height":444,"visible":false,"scale":"fill"}')
    $process.StandardInput.Flush()
    $deadline = [DateTime]::UtcNow.AddSeconds(3)
    do {
        [Windows.Forms.Application]::DoEvents()
        Start-Sleep -Milliseconds 20
        $visible = [EmbeddedViewerTest]::GetWindowLong($child, -16) -band 0x10000000
    } while ($visible -ne 0 -and [DateTime]::UtcNow -lt $deadline)
    if ($visible -ne 0) { throw 'Native viewport did not hide beneath the overlay.' }
    Write-Output 'Embedded viewer passed: real decoded picture, WS_CHILD parent, native resize and overlay visibility; remote input disabled.'
} finally {
    if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
    $process.Dispose()
    $form.Dispose()
}
