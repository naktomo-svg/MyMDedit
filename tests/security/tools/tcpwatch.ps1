param([Parameter(Mandatory)][string]$Out, [Parameter(Mandatory)][int]$Seconds)
# Polls the TCP table in a tight loop and logs each new connection to 192.0.2.0/24
# (TEST-NET-1, the SMB/WebDAV canaries in unc-leak.md). An SMB attempt shows up as a
# SynSent to 192.0.2.N:445 that lingers, since nothing answers. No admin rights needed,
# unlike the DNS cache or Process Monitor.
$props = [System.Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties()
$seen = @{}
$end = (Get-Date).AddSeconds($Seconds)
while ((Get-Date) -lt $end) {
    foreach ($c in $props.GetActiveTcpConnections()) {
        if ($c.RemoteEndPoint.Address.ToString().StartsWith('192.0.2.')) {
            $k = "$($c.LocalEndPoint)->$($c.RemoteEndPoint)"
            if (-not $seen.ContainsKey($k)) {
                $seen[$k] = 1
                "$(Get-Date -Format HH:mm:ss.fff) $($c.RemoteEndPoint) $($c.State)" | Add-Content $Out
            }
        }
    }
}
