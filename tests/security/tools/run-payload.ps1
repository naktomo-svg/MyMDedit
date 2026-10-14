param([Parameter(Mandatory)][string]$Name)
# Launches MyMDedit on tests/security/$Name.md with a WebView2 netlog and stderr captured.
$repo = (Resolve-Path "$PSScriptRoot\..\..\..").Path
$out = Join-Path $env:TEMP "mymdedit-security-out"
New-Item -ItemType Directory -Force $out | Out-Null
Get-Process MyMDedit -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 1500  # let the WebView2 browser process exit so new args apply
$netlog = Join-Path $out "$Name.netlog.json"
Remove-Item $netlog, "$out\$Name.stderr.txt" -ErrorAction SilentlyContinue
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--log-net-log=`"$netlog`" --net-log-capture-mode=Everything"
$p = Start-Process "$repo\target\debug\MyMDedit.exe" -ArgumentList "`"$repo\tests\security\$Name.md`"" `
    -RedirectStandardError "$out\$Name.stderr.txt" -RedirectStandardOutput "$out\$Name.stdout.txt" -PassThru -WindowStyle Normal
$p.Id

