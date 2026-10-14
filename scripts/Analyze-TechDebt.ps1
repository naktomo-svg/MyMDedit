[CmdletBinding()]
param(
    [string]$ClaudeCommand = 'claude',
    [switch]$DryRun,
    [string]$ReviewModel = 'opus',
    [ValidateSet('low', 'medium', 'high', 'xhigh', 'max')]
    [string]$ReviewEffort = 'high',
    [string]$MetaModel = 'opus',
    [ValidateSet('low', 'medium', 'high', 'xhigh', 'max')]
    [string]$MetaEffort = 'medium',
    [switch]$SkipClippy,
    [switch]$Fresh
)

$ErrorActionPreference = 'Stop'

function Get-RelativePath {
    param(
        [Parameter(Mandatory)] [string]$FullName,
        [Parameter(Mandatory)] [string]$RepoRoot
    )
    return ($FullName.Substring($RepoRoot.Length).TrimStart('\', '/')) -replace '\\', '/'
}

function Get-CacheKey {
    param(
        [Parameter(Mandatory)] [string]$Text
    )
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
    $hashBytes = [System.Security.Cryptography.SHA256]::HashData($bytes)
    return ([System.BitConverter]::ToString($hashBytes) -replace '-', '').Substring(0, 16).ToLowerInvariant()
}

function Write-CacheEntry {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)] [string]$Value
    )
    $tempPath = "$Path.tmp"
    Set-Content -LiteralPath $tempPath -Value $Value -Encoding utf8NoBOM -NoNewline
    Move-Item -LiteralPath $tempPath -Destination $Path -Force
}

function Get-TargetFiles {
    param(
        [Parameter(Mandatory)] [string]$RepoRoot
    )

    # target/ is cargo's build output; scripts/ is this tooling itself, not the app.
    $excludedDirNames = @('target', '.git', '.claude', 'docs', 'scripts')
    $targetExtensions = @('.rs', '.toml', '.rc', '.manifest')
    $results = [System.Collections.Generic.List[System.IO.FileInfo]]::new()

    function Walk-Directory([string]$dir) {
        foreach ($file in [System.IO.Directory]::EnumerateFiles($dir)) {
            $ext = [System.IO.Path]::GetExtension($file)
            if ($targetExtensions -contains $ext) {
                $results.Add([System.IO.FileInfo]::new($file))
            }
        }
        foreach ($subdir in [System.IO.Directory]::EnumerateDirectories($dir)) {
            $name = [System.IO.Path]::GetFileName($subdir)
            if ($excludedDirNames -notcontains $name) {
                Walk-Directory $subdir
            }
        }
    }

    Walk-Directory $RepoRoot

    return $results | Sort-Object { Get-RelativePath -FullName $_.FullName -RepoRoot $RepoRoot }
}

function Split-IntoBatches {
    param(
        [Parameter(Mandatory)] [System.IO.FileInfo[]]$Files,
        [int]$MaxFilesPerBatch = 10,
        [int]$MaxBytesPerBatch = 200KB
    )

    $batches = [System.Collections.Generic.List[System.IO.FileInfo[]]]::new()
    $current = [System.Collections.Generic.List[System.IO.FileInfo]]::new()
    $currentSize = 0L

    foreach ($file in $Files) {
        $wouldExceed = $current.Count -gt 0 -and (
            ($current.Count + 1) -gt $MaxFilesPerBatch -or
            ($currentSize + $file.Length) -gt $MaxBytesPerBatch
        )
        if ($wouldExceed) {
            $batches.Add($current.ToArray())
            $current = [System.Collections.Generic.List[System.IO.FileInfo]]::new()
            $currentSize = 0L
        }
        $current.Add($file)
        $currentSize += $file.Length
    }

    if ($current.Count -gt 0) {
        $batches.Add($current.ToArray())
    }

    return ,$batches.ToArray()
}

function Build-BatchContent {
    param(
        [Parameter(Mandatory)] [System.IO.FileInfo[]]$Files,
        [Parameter(Mandatory)] [string]$RepoRoot
    )

    $sb = [System.Text.StringBuilder]::new()
    foreach ($file in $Files) {
        $rel = Get-RelativePath -FullName $file.FullName -RepoRoot $RepoRoot
        [void]$sb.AppendLine("=== FILE: $rel ===")
        $lines = Get-Content -LiteralPath $file.FullName -Encoding utf8
        for ($i = 0; $i -lt $lines.Count; $i++) {
            [void]$sb.AppendLine("$($i + 1): $($lines[$i])")
        }
        [void]$sb.AppendLine()
    }
    return $sb.ToString()
}

# Runs `cargo clippy` and returns its diagnostics as plain text: free (no API call),
# and fed to the meta-analysis next to the AI findings. Returns $null when clippy could
# not run, so the report can say so instead of claiming the code is clean.
function Get-ClippyFindings {
    param(
        [Parameter(Mandatory)] [string]$RepoRoot
    )

    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if ($null -eq $cargo) {
        $fallback = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
        if (Test-Path -LiteralPath $fallback) { $cargo = $fallback } else { return $null }
    }

    Push-Location $RepoRoot
    try {
        # cargo prints diagnostics on stderr; merge it so they are captured.
        $output = & $cargo clippy --all-targets --message-format short --color never 2>&1 | ForEach-Object { "$_" }
        $exitCode = $LASTEXITCODE
    }
    finally {
        Pop-Location
    }

    # Keep only the diagnostic lines ("path:line:col: warning: ..."), not cargo's progress.
    $diagnostics = @($output | Where-Object { $_ -match '^\S+:\d+:\d+: (warning|error)' } | Sort-Object -Unique)
    if ($exitCode -ne 0 -and $diagnostics.Count -eq 0) {
        Write-Warning "cargo clippy failed (exit $exitCode) without diagnostics; omitting the static-analysis section."
        return $null
    }
    if ($diagnostics.Count -eq 0) {
        return 'clippyの指摘はありませんでした。'
    }
    return ($diagnostics -join "`n")
}

$script:ReviewPrompt = @'
あなたはRustとWin32 APIに精通したシニアレビュアーです。以下に渡されたソースコード(Win32 + WebView2で作られたWindows向けMarkdownエディタ)を読み、技術的負債をMarkdown形式で指摘してください。

あなたはこのバッチとして渡されたソースコードのみをレビューします。リポジトリのCLAUDE.mdやskill、docs/tech-debt/配下の既存レポートは参照せず、以下に渡された内容だけを根拠にしてください。指摘の検証や修正は行わず、指摘の列挙だけを行ってください。

各指摘は必ず次のフォーマットに従ってください:
- 場所: file:行番号
- 現象:
- 根本原因:
- 修正方針:
- 検証方法:
- コスト目安: S(1ファイル数十行以内) / M(複数ファイル) / L(設計変更)
- 優先度: 🔴 / 🟡 / 🟢
  - 🔴 高 = silent failure（Err/Resultの握り潰し等）・データ喪失・unsafeの健全性違反(未定義動作、ダングリングポインタ、寿命切れのバッファをWin32/COMへ渡す等)・RefCellの二重借用によるpanic・今のコードパスで再現できるバグ
  - 🟡 中 = 将来の機能追加で踏み抜きやすい設計の脆さ・観測性不足
  - 🟢 低 = 命名・抽象化・配置レベルの改善

次の指摘は禁止します:
- 型注釈追加の指摘
- ドキュメントコメント(///)追加の指摘
- コードフォーマット・様式美レベルの指摘(rustfmtやclippyのスタイル系lintで機械的に検出できるもの)

指摘が無い場合は、他に何も書かず次の一文だけを返してください: このバッチでは技術的負債は見つかりませんでした。
'@

function Invoke-ClaudeReview {
    param(
        [Parameter(Mandatory)] [string]$Content,
        [Parameter(Mandatory)] [string]$Prompt,
        [Parameter(Mandatory)] [string]$Model,
        [Parameter(Mandatory)] [string]$Effort,
        [string]$ClaudeCommand = 'claude'
    )

    $raw = $Content | & $ClaudeCommand --print --output-format json --model $Model --effort $Effort $Prompt
    if ($LASTEXITCODE -ne 0) {
        throw "$ClaudeCommand exited with code $LASTEXITCODE"
    }
    try {
        $parsed = $raw | ConvertFrom-Json
    }
    catch {
        throw "Could not parse $ClaudeCommand output as JSON: $($_.Exception.Message)"
    }
    if ($parsed.is_error) {
        throw "claude --print failed: $($parsed.result)"
    }
    if ([string]::IsNullOrWhiteSpace($parsed.result)) {
        throw "$ClaudeCommand returned an empty result"
    }
    return $parsed.result
}

$script:MetaPrompt = @'
以下は複数バッチに分けて実施した技術的負債レビューの指摘一覧です(バッチ区切りは "---" です)。末尾に「静的解析(clippy)」の節がある場合は、cargo clippy が機械的に出した警告の一覧です。全体を横串で見て、Markdown形式で次の3点をまとめてください。

リポジトリのCLAUDE.mdやskill、docs/tech-debt/配下の既存レポートは参照せず、以下に渡された指摘一覧だけを根拠にしてください。指摘の検証や修正は行わず、横断的な整理・要約のみを行ってください。clippyの警告は、AIの指摘と同じ箇所・同じ根本原因を指していれば統合し、単独では様式レベルのものはTOP5に入れないでください。

## 同じ根本原因の統合
似た指摘をグルーピングし、代表的な根本原因ごとにまとめてください。

## 優先対応TOP5
優先度・再現性・影響度を踏まえて、対応すべき指摘を最大5件挙げてください。

## コスト別の対応計画
S / M / L のコスト区分ごとに、対応方針をまとめてください。

指摘が1件もない場合は、その旨を一文で述べてください。
'@

function Build-MetaAnalysis {
    param(
        [Parameter(Mandatory)] [string]$AllFindings,
        [Parameter(Mandatory)] [string]$Model,
        [Parameter(Mandatory)] [string]$Effort,
        [string]$ClaudeCommand = 'claude'
    )
    return Invoke-ClaudeReview -Content $AllFindings -Prompt $script:MetaPrompt -Model $Model -Effort $Effort -ClaudeCommand $ClaudeCommand
}

function Write-TechDebtReport {
    param(
        [Parameter(Mandatory)] [string]$MetaAnalysis,
        [Parameter(Mandatory)] [object[]]$BatchSections,
        [string]$ClippyFindings,
        [Parameter(Mandatory)] [string]$OutputDir,
        [Parameter(Mandatory)] [string]$DateStamp,
        [Parameter(Mandatory)] [string]$ReviewModelLabel,
        [Parameter(Mandatory)] [string]$MetaModelLabel,
        [Parameter(Mandatory)] [string]$SourceSha,
        [Parameter(Mandatory)] [bool]$Complete
    )

    New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
    $reportPath = Join-Path $OutputDir "$DateStamp.md"
    $partialReportPath = Join-Path $OutputDir "$DateStamp-partial.md"
    $latestPath = Join-Path $OutputDir 'latest.md'

    $sb = [System.Text.StringBuilder]::new()
    [void]$sb.AppendLine("# 技術的負債レポート ($DateStamp)")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("- レビュー: $ReviewModelLabel")
    [void]$sb.AppendLine("- メタ分析: $MetaModelLabel")
    [void]$sb.AppendLine("- ソース: $SourceSha")
    if (-not $Complete) {
        [void]$sb.AppendLine('- ⚠ この実行は一部のステップが失敗しており、レポートは不完全です。')
    }
    [void]$sb.AppendLine()
    [void]$sb.AppendLine('## メタ分析')
    [void]$sb.AppendLine()
    [void]$sb.AppendLine($MetaAnalysis)
    [void]$sb.AppendLine()

    for ($i = 0; $i -lt $BatchSections.Count; $i++) {
        $section = $BatchSections[$i]
        [void]$sb.AppendLine("## Batch $($i + 1): $($section.FileList)")
        [void]$sb.AppendLine()
        [void]$sb.AppendLine($section.Findings)
        [void]$sb.AppendLine()
    }

    [void]$sb.AppendLine('## 静的解析(clippy)')
    [void]$sb.AppendLine()
    if ([string]::IsNullOrWhiteSpace($ClippyFindings)) {
        [void]$sb.AppendLine('(実行していません。-SkipClippy が指定されたか、cargo clippy を実行できませんでした。)')
    }
    else {
        [void]$sb.AppendLine('```')
        [void]$sb.AppendLine($ClippyFindings)
        [void]$sb.AppendLine('```')
    }
    [void]$sb.AppendLine()

    if ($Complete) {
        Set-Content -LiteralPath $reportPath -Value $sb.ToString() -Encoding utf8NoBOM -NoNewline
        Copy-Item -LiteralPath $reportPath -Destination $latestPath -Force
        if (Test-Path -LiteralPath $partialReportPath) {
            Remove-Item -LiteralPath $partialReportPath -Force
        }
        return $reportPath
    }
    else {
        Set-Content -LiteralPath $partialReportPath -Value $sb.ToString() -Encoding utf8NoBOM -NoNewline
        return $partialReportPath
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    $repoRoot = Split-Path -Parent $PSScriptRoot
    $files = Get-TargetFiles -RepoRoot $repoRoot
    $batches = Split-IntoBatches -Files $files

    $clippyFindings = $null
    if (-not $SkipClippy) {
        Write-Host 'Running cargo clippy...'
        $clippyFindings = Get-ClippyFindings -RepoRoot $repoRoot
    }

    if ($DryRun) {
        for ($i = 0; $i -lt $batches.Count; $i++) {
            $batch = $batches[$i]
            $size = ($batch | Measure-Object -Property Length -Sum).Sum
            Write-Host "Batch $($i + 1) ($($batch.Count) files, $size bytes):"
            foreach ($f in $batch) {
                $rel = Get-RelativePath -FullName $f.FullName -RepoRoot $repoRoot
                Write-Host "  $rel"
            }
        }
        Write-Host "Total: $($files.Count) files in $($batches.Count) batches"
        if ($null -ne $clippyFindings) {
            $count = @($clippyFindings -split "`n" | Where-Object { $_ -match ': (warning|error)' }).Count
            Write-Host "clippy: $count diagnostic(s)"
        }
        return
    }

    $outputDir = Join-Path $repoRoot 'docs/tech-debt'
    $partialDir = Join-Path $outputDir '.partial'
    New-Item -ItemType Directory -Force -Path $partialDir | Out-Null

    $allSucceeded = $true
    $batchResults = [System.Collections.Generic.List[string]]::new()
    $batchSections = [System.Collections.Generic.List[object]]::new()
    for ($i = 0; $i -lt $batches.Count; $i++) {
        $batch = $batches[$i]
        $content = Build-BatchContent -Files $batch -RepoRoot $repoRoot
        $key = Get-CacheKey -Text ($script:ReviewPrompt + "`n" + $ReviewModel + "`n" + $ReviewEffort + "`n" + $ClaudeCommand + "`n" + $content)
        $cachePath = Join-Path $partialDir "batch-$($i + 1)-$key.md"

        $result = $null
        if (-not $Fresh -and (Test-Path -LiteralPath $cachePath)) {
            $cached = Get-Content -LiteralPath $cachePath -Raw
            if ([string]::IsNullOrWhiteSpace($cached)) {
                Write-Warning "Cached result for batch $($i + 1) is empty; ignoring it and re-running."
            }
            else {
                Write-Host "Batch $($i + 1)/$($batches.Count): reusing cached result"
                $result = $cached
            }
        }

        if ($null -eq $result) {
            Write-Host "Reviewing batch $($i + 1)/$($batches.Count) ($($batch.Count) files) [$ReviewModel/$ReviewEffort]..."
            $reviewOk = $false
            try {
                $result = Invoke-ClaudeReview -Content $content -Prompt $script:ReviewPrompt -Model $ReviewModel -Effort $ReviewEffort -ClaudeCommand $ClaudeCommand
                $reviewOk = $true
            }
            catch {
                Write-Warning "Batch $($i + 1) failed: $($_.Exception.Message). Recording as failed and continuing."
                $result = "(このバッチのレビューに失敗しました: $($_.Exception.Message))"
                $allSucceeded = $false
            }

            if ($reviewOk) {
                try {
                    Write-CacheEntry -Path $cachePath -Value $result
                }
                catch {
                    Write-Warning "Could not cache batch $($i + 1) result: $($_.Exception.Message). Continuing without caching it."
                }
            }
        }

        $batchResults.Add($result)
        $fileList = ($batch | ForEach-Object {
            Get-RelativePath -FullName $_.FullName -RepoRoot $repoRoot
        }) -join ', '
        $batchSections.Add([pscustomobject]@{ FileList = $fileList; Findings = $result })
    }

    $metaAnalysis = $null
    if ($allSucceeded) {
        $labelled = for ($i = 0; $i -lt $batchResults.Count; $i++) {
            "## Batch $($i + 1)`n`n$($batchResults[$i])"
        }
        $allFindings = $labelled -join "`n`n---`n`n"
        if (-not [string]::IsNullOrWhiteSpace($clippyFindings)) {
            $allFindings += "`n`n---`n`n## 静的解析(clippy)`n`n$clippyFindings"
        }

        $metaKey = Get-CacheKey -Text ($script:MetaPrompt + "`n" + $MetaModel + "`n" + $MetaEffort + "`n" + $ClaudeCommand + "`n" + $allFindings)
        $metaCachePath = Join-Path $partialDir "meta-$metaKey.md"

        if (-not $Fresh -and (Test-Path -LiteralPath $metaCachePath)) {
            $cachedMeta = Get-Content -LiteralPath $metaCachePath -Raw
            if ([string]::IsNullOrWhiteSpace($cachedMeta)) {
                Write-Warning 'Cached meta-analysis result is empty; ignoring it and re-running.'
            }
            else {
                Write-Host 'Meta-analysis: reusing cached result'
                $metaAnalysis = $cachedMeta
            }
        }

        if ($null -eq $metaAnalysis) {
            Write-Host "Running meta-analysis [$MetaModel/$MetaEffort]..."
            $metaOk = $false
            try {
                $metaAnalysis = Build-MetaAnalysis -AllFindings $allFindings -Model $MetaModel -Effort $MetaEffort -ClaudeCommand $ClaudeCommand
                $metaOk = $true
            }
            catch {
                Write-Warning "Meta-analysis failed: $($_.Exception.Message). Writing report with batch findings only."
                $metaAnalysis = "(メタ分析に失敗しました: $($_.Exception.Message))"
                $allSucceeded = $false
            }

            if ($metaOk) {
                try {
                    Write-CacheEntry -Path $metaCachePath -Value $metaAnalysis
                }
                catch {
                    Write-Warning "Could not cache meta-analysis result: $($_.Exception.Message). Continuing without caching it."
                }
            }
        }
    }
    else {
        Write-Host 'Skipping meta-analysis because at least one batch failed.'
        $metaAnalysis = '(一部のバッチが失敗したため、メタ分析はスキップされました。失敗したバッチを再実行してから再度実行してください。)'
    }

    try {
        $sourceSha = (git -C $repoRoot rev-parse --short HEAD 2>$null)
    }
    catch {
        $sourceSha = $null
    }
    if ([string]::IsNullOrWhiteSpace($sourceSha)) { $sourceSha = '(unknown)' }
    else {
        try {
            $dirtyStatus = (git -C $repoRoot status --porcelain 2>$null)
        }
        catch {
            $dirtyStatus = $null
        }
        if (-not [string]::IsNullOrWhiteSpace($dirtyStatus)) {
            $sourceSha = "$sourceSha-dirty"
        }
    }

    $dateStamp = Get-Date -Format 'yyyy-MM-dd'
    $reportPath = Write-TechDebtReport -MetaAnalysis $metaAnalysis -BatchSections $batchSections.ToArray() -ClippyFindings $clippyFindings -OutputDir $outputDir -DateStamp $dateStamp -ReviewModelLabel "$ReviewModel / $ReviewEffort" -MetaModelLabel "$MetaModel / $MetaEffort" -SourceSha $sourceSha -Complete $allSucceeded

    Write-Host "Report written to $reportPath"
    Write-Host "Latest copy: $(Join-Path $outputDir 'latest.md')"

    if ($allSucceeded) {
        Remove-Item -LiteralPath $partialDir -Recurse -Force
    }
    else {
        Write-Host "Some steps failed; keeping $partialDir so a re-run retries only the failed steps."
    }

    if (-not $allSucceeded) {
        exit 1
    }
}
