# MyMDedit

Windows向けのシンプルなMarkdownエディタです。左側でMarkdownを編集すると、右側にHTMLプレビューがリアルタイムに表示されます。

Win32 API（`windows`クレート）とWebView2で作った、単一の`.exe`だけで動くネイティブアプリです。.NETランタイムやVC++ランタイム、MSIXのインストールは要りません（必要なのはWebView2ランタイムだけです）。

移植元は、同じ作者によるWinUI 3製のMarkdownエディタ「MyWinApp」です（非公開・開発終了）。動作の基準にしているMyWinAppの仕様書を[`docs/spec.md`](docs/spec.md)に収録しています。

## 主な機能

- **編集・プレビュー**: 左右分割、入力ごとにリアルタイム変換。エディタのスクロール位置に合わせてプレビューも割合で追従します
- **安全なプレビュー**: スクリプトは実行せず、画面遷移・ダウンロード・外部アプリの起動も行いません（CSP、`IsScriptEnabled=false`、WebView2のリクエストフィルターとナビゲーションガード）。外部への通信は、`https://`の画像の読み込みだけを許しています。そのため、文書に外部の画像があると、文書を開いたことが画像の置き場所のサーバーに伝わります。`<link>`・`<meta>`・`<iframe>`など、取得や接続のためだけにあるHTML要素は、要素として扱わずに文字として表示します
- **コードのコピー**: プレビューのコードブロックにマウスを乗せると右上に「コピー」ボタンが出て、クリックするとそのブロックの中身をクリップボードにコピーします（改行はCRLF）。スクリプトを使わず、アプリ側でコピーします
- **ズーム**: Ctrl+マウスホイール（エディタ上）、Ctrl+`+` / Ctrl+`-` / Ctrl+`0`。50%〜200%で、エディタとプレビューが連動します（起動するたびに100%に戻ります）
- **フッター**: 行数・文字数・ズーム倍率を表示します
- **Markdown記法ツールバー**: 太字・斜体・見出し(H1〜H3)・箇条書き・番号付きリスト・リンク・コード。「その他」には引用・水平線・画像・テーブル・取り消し線・コードブロック・タスクリストがあります
- **リストの自動継続**: `- `・`N. `・`- [ ] `の行でEnterを押すと、同じ字下げで次の行にも記法が付きます（番号は連番、タスクは未チェック）。記法だけの行でEnterを押すとリストを抜けます
- **リストの字下げ**: リストの行でTabを押すと空白4つ分字下げし、Shift+Tabで戻します。複数行を選択していればまとめて字下げします（リスト以外の行ではTabはタブ文字のままです）
- **タスクリスト**: Ctrl+Enter（または「編集」→「タスクの切り替え」）で`- [ ]`と`- [x]`を切り替えます。リストや普通の行は`- [ ]`の行になります
- **見出しへ移動**: Ctrl+Shift+Oで文書中の見出しを一覧し、選んだ見出しへ移動します（コードブロックの中の`#`は見出しとして扱いません）
- **行へ移動**: Ctrl+Gで行番号を指定して移動します（全角数字も入力できます）
- **ファイル**: 新規 / 開く / 保存 / 名前を付けて保存 / 最近使ったフォルダ(3件) / 終了
  - 文字コード（BOMの有無を含む）と改行コード（CRLF / LF / CR）は、開いたときのまま保存します
  - Shift-JISなどUTF-8として読めないファイルは開かずにエラーを表示します（元のファイルには触れません）
  - 保存は一時ファイルに書いてから差し替えるので、保存中に失敗しても元のファイルは壊れません
  - 未保存の変更があるときは「保存する / 保存しない / キャンセル」を確認します
  - 開いているファイルが別のアプリで変更されると、ウィンドウに戻ったときに「読み込み直す / このまま」を確認します。削除・移動されたときはエラー表示で知らせます
  - 開いたあとに別のアプリで変更されたファイルに保存するときは、上書きしてよいかを確認します
- **検索・置換**: Windows標準の検索/置換ダイアログを使います（大文字と小文字を区別する・単語単位で検索する）
- **設定**: テーマ（システムの既定 / ライト / ダーク）、エディタ・プレビューのフォントとサイズ、プレビューの背景色（Windows標準の色の設定ダイアログ）、表のスタイル

## 動作環境

- Windows 10 バージョン1809以降 / Windows 11（x64）
- [WebView2 ランタイム](https://developer.microsoft.com/microsoft-edge/webview2/)（多くの環境には最初から入っています）

## ビルド・実行

```powershell
cargo build --release
.\target\release\MyMDedit.exe            # 起動
.\target\release\MyMDedit.exe memo.md    # ファイルを開いて起動
cargo test                               # 単体テスト
```

`.md`ファイルをダブルクリックで開きたいときは、エクスプローラーで`.md`ファイルを右クリックし、「プログラムから開く」→「別のプログラムを選択」で`MyMDedit.exe`を選んでください（MSIXを使わないため、アプリ側では関連付けを登録しません）。

## 技術負債の検査

AIレビュー（`claude`コマンド）とclippyを組み合わせて技術負債レポートを作ります。`docs/tech-debt/<日付>.md`と`latest.md`に出力されます（リポジトリには含めません）。[Claude Code](https://claude.com/claude-code)の`claude`コマンドが必要です。

```powershell
pwsh ./scripts/Analyze-TechDebt.ps1 -DryRun   # 対象ファイルの分け方とclippyの件数だけ確認（API呼び出しなし）
pwsh ./scripts/Analyze-TechDebt.ps1           # 実行（Opusを使うためAPI費用がかかります。中断しても再実行で続きから再開）
```

レポートの指摘に対応するときの手順は`.claude/skills/`（verify-then-fix → why5-and-horizontal-check）と`CLAUDE.md`にまとめてあります。

## 保存場所

- 設定: `%APPDATA%\MyMDedit\settings.json`
- WebView2のデータ: `%LOCALAPPDATA%\MyMDedit\WebView2`
- クラッシュ時の記録: `%LOCALAPPDATA%\MyMDedit\crash.log`

## 移植元（MyWinApp）との違い

| 項目 | MyWinApp | MyMDedit |
|---|---|---|
| UI | WinUI 3（XAML） | Win32ネイティブコントロール |
| エディタ | `TextBox` | RichEdit（プレーンテキストモード。複数回の元に戻す/やり直しに対応） |
| Markdown変換 | Markdig | comrak（GFM拡張とCJK対応の強調を有効化） |
| ツールバーの位置 | メニューバーの右隣 | メニューバーの下の行 |
| 見出し・その他のメニュー | 項目ごとのツールチップで記法を表示 | メニュー項目の右側に記法を表示 |
| 最近使ったフォルダ | フルパスはツールチップ | フルパスはメニュー項目の右側に表示 |
| 検索・置換 | 自作ダイアログ（前を検索・次を検索ボタン） | Windows標準ダイアログ（上へ/下へで方向を選ぶ） |
| 設定の保存先 | `ApplicationData.LocalSettings` | `%APPDATA%\MyMDedit\settings.json` |
| `.md`の関連付け | MSIXで自動登録 | 「プログラムから開く」で手動設定 |
| コードブロックのコピーボタン | なし | あり |
| テスト | なし | 純粋ロジック（改行・文字コード・記法挿入・検索・設定・HTML生成）に単体テスト |

## プロジェクト構成

```
src/
├── main.rs             エントリポイント（COM初期化、コマンドライン引数）
├── app.rs              メインウィンドウ・メニュー・ツールバー・各コマンド
├── editor.rs           RichEditのラッパー
├── preview.rs          WebView2のホスト、リクエストフィルター、ナビゲーションガード
├── render.rs           Markdown → HTML文書（CSP・テーマCSS）
├── md_ops.rs           記法挿入・リスト自動継続・字下げ・見出し抽出（純粋関数）
├── search.rs           検索・置換のマッチング（純粋関数）
├── document.rs         文字コード・BOMの判定、アトミック保存
├── line_endings.rs     改行コードの定義（判定・正規化・行数）
├── settings.rs         設定の読み書き（上下限の定義を含む）
├── settings_dialog.rs  設定ダイアログ
├── dialogs.rs          ファイル選択・確認・色選択ダイアログ、フォント列挙
├── goto_dialog.rs      行へ移動ダイアログ
├── theme.rs            ダークモード（タイトルバー・メニューバー描画）
├── win.rs              Win32の小さなヘルパー
└── log.rs              エラーの記録先（OutputDebugString）
res/                    アイコンとアプリケーションマニフェスト
docs/spec.md            移植元（MyWinApp）の仕様書
fuzz/                   純粋ロジックのファズテスト（cargo-fuzz）
tests/security/         プレビューのセキュリティ回帰テスト（攻撃用ペイロードと検査ツール。Pythonが必要）
scripts/                技術負債レポートの生成スクリプト
.claude/skills/         技術負債への対応手順（Claude Code用スキル）
```

## ライセンス

[MIT License](LICENSE)（Copyright (c) 2026 あぽろ）

`MyMDedit.exe`に含まれる第三者のソフトウェア（Rustのクレートと、静的リンクしているMicrosoft WebView2 SDKのローダー）のライセンスは[`THIRD-PARTY-NOTICES.txt`](THIRD-PARTY-NOTICES.txt)にまとめてあります。依存関係を変えたときは`cargo about generate about.hbs -o THIRD-PARTY-NOTICES.txt`で作り直してください。
