# 実装役(implementer)の PreToolUse フック(Edit / Write / NotebookEdit)。
# 実装役が書き換えてよいのは、統括役が割り当てた worktree(.claude/worktrees/<タスクID>/)の中だけ。
# その中でも、チームの設定・フック・CI の定義は人の承認なしに変えさせない。

$ErrorActionPreference = 'Stop'
# Windows の既定のコードページのままだと、日本語の理由やパスが文字化けする
[Console]::InputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
try {
    $payload = [Console]::In.ReadToEnd() | ConvertFrom-Json
    $path = [string]$payload.tool_input.file_path
    if (-not $path) { $path = [string]$payload.tool_input.notebook_path }
} catch {
    [Console]::Error.WriteLine("guard-edit: フックの入力を読めなかったため、止めました: $_")
    exit 2
}
if (-not $path) {
    [Console]::Error.WriteLine('guard-edit: 書き込み先のパスが分からないため、止めました')
    exit 2
}

$project = $env:CLAUDE_PROJECT_DIR
if (-not $project) { $project = [string]$payload.cwd }
$full = [System.IO.Path]::GetFullPath($path, $project)
$root = [System.IO.Path]::GetFullPath((Join-Path $project '.claude/worktrees')) + [System.IO.Path]::DirectorySeparatorChar

if (-not $full.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
    [Console]::Error.WriteLine("guard-edit: 割り当てられた worktree の外には書き込めません: $full")
    exit 2
}

# worktree の中のパス(タスクIDのディレクトリより下)
$inside = $full.Substring($root.Length) -replace '\\', '/'
$inside = ($inside -split '/', 2)[1]

$protected = @(
    '^\.claude/'                 # エージェント定義・フック・設定
    '^\.github/workflows/'       # CI の定義
    '^\.mcp\.json$'              # MCP サーバーの設定
    '(^|/)\.env(\.|$)'           # 秘密情報
)
foreach ($re in $protected) {
    if ($inside -imatch $re) {
        [Console]::Error.WriteLine("guard-edit: $inside はチームの安全設定にかかわるファイルのため、実装役は変更できません。必要なら報告に書いてください")
        exit 2
    }
}
exit 0
