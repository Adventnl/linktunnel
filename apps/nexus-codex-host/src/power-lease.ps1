$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System.Runtime.InteropServices;
public static class NexusMobilePower {
    [DllImport("kernel32.dll")]
    public static extern uint SetThreadExecutionState(uint flags);
}
'@
$continuous = [uint32]2147483648
$systemRequired = [uint32]1
if ([NexusMobilePower]::SetThreadExecutionState($continuous -bor $systemRequired) -eq 0) { exit 1 }
try {
    # EOF also releases the lease when Node exits unexpectedly. This process owns no UI.
    $null = [Console]::ReadLine()
} finally {
    $null = [NexusMobilePower]::SetThreadExecutionState($continuous)
}
