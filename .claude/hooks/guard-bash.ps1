# PreToolUse フック(Bash / PowerShell 共通)。
# 標準入力の JSON から tool_input.command を取り出し、人の承認なしに実行させない操作なら exit 2 で止める。
# exit 2 のとき、標準エラーの文章が理由として Claude に返る。
# settings.json の deny 規則と二重にしているのは、deny のワイルドカードでは書き方の揺れ
# (オプションの順番、PowerShell の別名など)を拾いきれないため。

$ErrorActionPreference = 'Stop'
# Windows の既定のコードページのままだと、日本語の理由が文字化けして Claude に届く
[Console]::InputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
try {
    $payload = [Console]::In.ReadToEnd() | ConvertFrom-Json
    $command = [string]$payload.tool_input.command
} catch {
    # 入力を読めないときは止める(判断できないまま通さない)
    [Console]::Error.WriteLine("guard-bash: フックの入力を読めなかったため、止めました: $_")
    exit 2
}
if ([string]::IsNullOrWhiteSpace($command)) { exit 0 }

$rules = @(
    # --- 破壊的なファイル操作 ---
    @{ re = '\brm\s+(-[a-z]*r[a-z]*f|-[a-z]*f[a-z]*r|-r\s+-f|-f\s+-r|--recursive)';  why = '再帰的な削除(rm -rf)は人の承認が必要です' }
    @{ re = '\b(Remove-Item|rmdir|rd|del|ri)\b[^|;]*\s-Recurse';                     why = '再帰的な削除(Remove-Item -Recurse)は人の承認が必要です' }
    # --- git の履歴やほかの作業を消す操作 ---
    @{ re = '\bgit\s+push\b[^|;&]*(\s--force\b|\s-f\b|\s--force-with-lease\b|\s\+\S)'; why = '強制 push は禁止です' }
    @{ re = '\bgit\s+reset\s+[^|;&]*--hard';                                          why = 'git reset --hard は作業を消すため禁止です。git revert で戻してください' }
    @{ re = '\bgit\s+clean\s+-[a-z]*f';                                               why = 'git clean -f は追跡外のファイルを消すため禁止です' }
    @{ re = '\bgit\s+branch\s+[^|;&]*(-D\b|--delete\s+--force)';                      why = 'ブランチの強制削除は人の承認が必要です' }
    @{ re = '\bgit\s+worktree\s+remove\b[^|;&]*--force';                              why = '変更が残った worktree の強制削除は人の承認が必要です' }
    @{ re = '\bgit\s+checkout\s+[^|;&]*\s--\s+\.';                                    why = '作業ツリー全体の変更を捨てる操作は禁止です' }
    @{ re = '\bgit\s+(rebase|filter-branch|filter-repo)\b';                           why = '履歴の書き換えは人の承認が必要です' }
    # --- テストや検査の回避 ---
    @{ re = '--no-verify\b';                                                          why = 'フックや検査を飛ばす --no-verify は禁止です' }
    @{ re = 'commit\.gpgsign=false|--no-gpg-sign';                                    why = '署名の回避は禁止です' }
    # --- main への統合・公開(人が行う) ---
    @{ re = '\bgh\s+pr\s+merge\b';                                                    why = 'PR のマージは人が行います' }
    @{ re = '\bgh\s+release\s+(create|delete|edit)\b';                                why = 'リリースの作成・変更は人の承認が必要です' }
    @{ re = '\bgit\s+merge\b[^|;&]*\b(main|master)\b';                                why = 'main への統合は PR を通して人が行います' }
    # --- データベース・スキーマ ---
    @{ re = '\b(DROP\s+(TABLE|DATABASE|SCHEMA)|TRUNCATE\s+TABLE)\b';                  why = 'テーブルの削除は禁止です' }
    @{ re = '\b(prisma\s+migrate\s+(deploy|reset)|alembic\s+upgrade|rails\s+db:(migrate|drop|reset)|diesel\s+migration\s+run|flyway\s+migrate|knex\s+migrate:latest)\b'; why = 'スキーマの移行は人の承認が必要です' }
    # --- 秘密情報の表示 ---
    @{ re = '^\s*(env|printenv|set)\s*$|\bprintenv\b|Get-ChildItem\s+env:|\bgci\s+env:|\bls\s+env:'; why = '環境変数の一覧はログに秘密が残るため禁止です' }
    @{ re = '(\$env:|\$\{?)\w*(KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL)\w*';     why = '秘密情報らしい環境変数を参照するコマンドは禁止です' }
    @{ re = '\b(cat|type|Get-Content|gc|more|less|head|tail)\b[^|;&]*(\.env\b|\.pem\b|\.key\b|id_rsa|id_ed25519|credentials)'; why = '秘密情報のファイルを表示するのは禁止です' }
)

foreach ($rule in $rules) {
    if ($command -imatch $rule.re) {
        [Console]::Error.WriteLine("guard-bash: $($rule.why)。必要なら作業を止め、統括役を通して人に承認を求めてください。(該当: $($Matches[0]))")
        exit 2
    }
}
exit 0
