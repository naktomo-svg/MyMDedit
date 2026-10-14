param([string[]]$Only)
# Opens every payload in tests/security in the debug build, does the clicks, and checks
# the outcome — one PASS/FAIL line per payload, exit code 1 on any failure.
#
# Each payload gets a fresh APPDATA/LOCALAPPDATA under %TEMP%\mymdedit-security-out, so
# the user's settings and WebView2 profile are never touched and no state carries over.
# Observation: WebView2 netlog (check_netlog.py), a TCP listener on 127.0.0.1:18440-18459
# (listener.py), a TCP-table watcher for 192.0.2.0/24 (tcpwatch.ps1), the app's stderr
# (log::error) and the Downloads folder. Every payload with a positive control fails as
# "observation broken" when the control does not show, so a blind run never passes.
#
# Needs: `cargo build` done, `winapp` and `py` on PATH, an unlocked desktop (the clicks
# are real mouse input). Takes about 5 minutes.

$repo = (Resolve-Path "$PSScriptRoot\..\..\..").Path
$exe = "$repo\target\debug\MyMDedit.exe"
$tools = $PSScriptRoot
$outRoot = Join-Path $env:TEMP "mymdedit-security-out"
if (-not (Test-Path $exe)) { throw "build first: cargo build" }

# clicks: link texts that must be clicked (a click that cannot be made is a failure:
# an unclicked link passes vacuously). optional: may be absent (e.g. rendered as text).
$cases = [ordered]@{
    'p2-passive'        = @{ netRequire = @('ctrl-img') }
    'f1-download-data'  = @{ clicks = @('F1DATA'); stderr = @('PreviewDownloadBlocked'); noDownloads = $true }
    'f1-download-txt'   = @{ clicks = @('F1TXT'); stderr = @('PreviewDownloadBlocked'); noDownloads = $true }
    'f1-download-https' = @{ clicks = @('F1HTTPS'); stderr = @('Preview(Navigation|Request)Blocked'); noDownloads = $true }
    'f3-iframe'         = @{ optional = @('F3SELF', 'F3DATA', 'F3BLANK', 'F3TOP') }
    'nav-click'         = @{ clicks = @('NAVHTTPS', 'NAVMSSETTINGS', 'NAVMAILTO', 'NAVFILE', 'NAVUNC', 'NAVDATA', 'NAVBLANK')
                             stderr = @('PreviewNavigationBlocked') }
    'local-leak'        = @{ clicks = @('CLICKPLAIN', 'CLICKHTTPS'); listener = $true }
    'unc-leak'          = @{ clicks = @('UNCFILE31', 'UNCBS32', 'MDLINK33', 'SEARCHMS34', 'MSWORD35'); tcpRequire = '192.0.2.25' }
}
# Apps a blocked link would have started.
$launchWatch = 'SystemSettings', 'olk', 'OUTLOOK', 'HxOutlook', 'WINWORD', 'SearchApp', 'notepad', 'CalculatorApp'

Add-Type -Namespace SecTest -Name Win -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr h);
[DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, System.UIntPtr extra);
'@

# winapp refuses to click a window that is not in the foreground, and UIA focus does not
# make it so. A synthetic Alt press lifts the foreground lock for SetForegroundWindow.
function Set-AppForeground {
    $h = (Get-Process MyMDedit -ErrorAction SilentlyContinue | Select-Object -First 1).MainWindowHandle
    if (-not $h) { return }
    [SecTest.Win]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
    [SecTest.Win]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
    [void][SecTest.Win]::SetForegroundWindow($h)
    Start-Sleep -Milliseconds 300
}

function Invoke-Click([string]$text) {
    Set-AppForeground
    winapp ui focus 300 -a MyMDedit 2>&1 | Out-Null
    winapp ui scroll-into-view $text -a MyMDedit 2>&1 | Out-Null
    Start-Sleep -Milliseconds 500
    $r = winapp ui click $text -a MyMDedit 2>&1 | Out-String
    # Matched on text: the ✅ winapp prints is mangled when the console is not UTF-8.
    $ok = $r -match '\bclick on \S+ at \('
    if (-not $ok -and $r -match 'not in the foreground') {
        # Locked screen or foreground lock: UIA Invoke still clicks the link (the page
        # sees a click), just not through the mouse.
        $ok = (winapp ui invoke $text -a MyMDedit 2>&1 | Out-String) -match 'Invoked'
        if ($ok) { $script:invoked.Add($text) }
    }
    Start-Sleep -Seconds 3
    return $ok
}

$results = [ordered]@{}
foreach ($name in $cases.Keys) {
    if ($Only -and $name -notin $Only) { continue }
    $c = $cases[$name]
    $fail = [System.Collections.Generic.List[string]]::new()
    $script:invoked = [System.Collections.Generic.List[string]]::new()
    $dir = Join-Path $outRoot $name
    if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    New-Item -ItemType Directory -Force "$dir\Roaming", "$dir\Local" | Out-Null
    Get-Process MyMDedit -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 2

    $listenerLog = "$dir\listener.txt"; $tcpLog = "$dir\tcp.txt"; $netlog = "$dir\netlog.json"
    $listener = Start-Process py -ArgumentList '-I', "$tools\listener.py", $listenerLog, '90' -PassThru -WindowStyle Hidden
    $watch = Start-Process pwsh -ArgumentList '-NoProfile', '-File', "$tools\tcpwatch.ps1", $tcpLog, '90' -PassThru -WindowStyle Hidden
    $before = Get-Process | Select-Object -ExpandProperty Id
    $started = Get-Date
    Start-Sleep -Seconds 2

    # Only the app gets the isolated profile: py and winapp keep their state under
    # LOCALAPPDATA too, and break (or reinstall themselves) when it moves.
    $saved = $env:APPDATA, $env:LOCALAPPDATA
    $env:APPDATA = "$dir\Roaming"; $env:LOCALAPPDATA = "$dir\Local"
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--log-net-log=`"$netlog`" --net-log-capture-mode=Everything"
    $app = Start-Process $exe -ArgumentList "`"$repo\tests\security\$name.md`"" -PassThru `
        -RedirectStandardError "$dir\stderr.txt" -RedirectStandardOutput "$dir\stdout.txt"
    $env:APPDATA, $env:LOCALAPPDATA = $saved
    Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS
    Start-Sleep -Seconds 8

    foreach ($t in @($c.clicks)) { if ($t -and -not (Invoke-Click $t)) { $fail.Add("could not click $t") } }
    foreach ($t in @($c.optional)) { if ($t) { [void](Invoke-Click $t) } }
    Start-Sleep -Seconds 4

    $launched = Get-Process | Where-Object { $_.Id -notin $before -and $_.ProcessName -in $launchWatch }
    foreach ($p in $launched) { $fail.Add("started $($p.ProcessName)") }

    # Close normally so the browser process flushes the netlog.
    [void]$app.CloseMainWindow()
    if (-not $app.WaitForExit(15000)) { $app.Kill() }
    Start-Sleep -Seconds 5
    Stop-Process $listener.Id, $watch.Id -Force -ErrorAction SilentlyContinue

    # netlog
    $netArgs = @($netlog) + (@($c.netRequire) | Where-Object { $_ } | ForEach-Object { '--allow', $_, '--require', $_ })
    if (Test-Path $netlog) {
        py -I "$tools\check_netlog.py" @netArgs | ForEach-Object { Write-Host $_ }
        if ($LASTEXITCODE -ne 0) { $fail.Add('netlog') }
    } else { $fail.Add('no netlog (observation broken)') }

    # stderr
    $err = Get-Content "$dir\stderr.txt" -ErrorAction SilentlyContinue | Where-Object { $_ -notmatch 'superseded or blocked navigation' }
    foreach ($pat in @($c.stderr)) { if ($pat -and -not ($err -match $pat)) { $fail.Add("stderr lacks $pat") } }
    if ($err -match 'panic|PreviewSecurity|安全確認に失敗') { $fail.Add('preview stopped or panicked') }

    # F16: the preview is InPrivate, so the profile never keeps the document. The page is
    # a data: URI of the whole render; a persistent profile wrote it into History.
    $profileHits = Get-ChildItem "$dir\Local\MyMDedit\WebView2" -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { [Text.Encoding]::Latin1.GetString([IO.File]::ReadAllBytes($_.FullName)).Contains('data:text/html') }
    foreach ($f in $profileHits) { $fail.Add("document persisted in profile: $($f.Name)") }

    # Downloads
    if ($c.noDownloads) {
        $dl = Get-ChildItem "$env:USERPROFILE\Downloads" -Filter 'mymdedit-*' -ErrorAction SilentlyContinue |
            Where-Object LastWriteTime -gt $started
        foreach ($f in $dl) { $fail.Add("downloaded $($f.Name)") }
    }

    # Local listener: plaintext request bytes never arrive; TLS only from the accepted
    # https image (18440) and the clicked https link's pre-connect (18448, F8).
    if ($c.listener) {
        $lines = Get-Content $listenerLog -ErrorAction SilentlyContinue
        if (-not ($lines -match 'CONNECT port=18440')) { $fail.Add('listener: control 18440 missing (observation broken)') }
        foreach ($l in $lines -match 'DATA') {
            if ($l -match 'tls=False' -or $l -notmatch 'port=184(40|48)') { $fail.Add("listener: $l") }
        }
    }

    # TCP watcher: the only 192.0.2.x connection is the https control image.
    if ($c.tcpRequire) {
        $lines = Get-Content $tcpLog -ErrorAction SilentlyContinue
        if (-not ($lines -match [regex]::Escape("$($c.tcpRequire):443"))) { $fail.Add('tcp: control missing (observation broken)') }
        foreach ($l in $lines) { if ($l -notmatch [regex]::Escape("$($c.tcpRequire):443")) { $fail.Add("tcp: $l") } }
    }

    $how = if ($invoked.Count) { " (UIA Invoke, not mouse: $($invoked -join ', '))" } else { '' }
    $results[$name] = if ($fail.Count) { "FAIL: $($fail -join '; ')$how" } else { "PASS$how" }
    Write-Host "$name : $($results[$name])"
}

Write-Host "`n=== summary"
$results.GetEnumerator() | ForEach-Object { Write-Host ("{0,-18} {1}" -f $_.Key, $_.Value) }
exit ([int]($results.Values -match '^FAIL').Count -gt 0)
