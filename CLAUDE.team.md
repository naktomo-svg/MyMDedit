# チーム運用(このファイルは CLAUDE.md から `@CLAUDE.team.md` で読み込む)

> サブエージェント(implementer / reviewer)への注記: 「統括役」の節はメインセッション向けです。あなたは自分のエージェント定義に従い、この節の手順は実行しないでください。「Commands」と「セキュリティ再監査」の節は共通で使います。

## Commands

チームが使ってよい検証コマンドと合格の基準は、CLAUDE.md の「Commands」節と、その直後の検証の基準(build / test / clippy / fmt --check)だけです。ここには書き写しません(ツールの状態を書くのは CLAUDE.md の一か所だけ、という規則のため)。

- 各 worktree は自分の `target\` を持つため、初回のビルドは時間がかかります。
- `cargo` が見つからないときは、`%USERPROFILE%\.cargo\bin` を PATH の先頭に足します。
- `fuzz/` を変えたときは、そこでも `cargo fmt --check` を実行します。

## セキュリティ再監査

条件と最低限のコマンドは、CLAUDE.md の「Development workflow」節にある Security re-audit triggers に従います。
- 条件: `webview2-com` や `comrak` の更新、`preview.rs` のハンドラや設定の変更、内容やパスがアプリに入る新しい経路、WebView2 Runtime の大きな更新。
- 実行するもの: `run-all.ps1`(debug ビルド、約5分。`run_in_background` で回してよい)、`cargo deny check`、変更したモジュールに対応する fuzz ターゲット。
- 結果はコミットメッセージか PR の説明に残します。

## このリポジトリの固有の規則(実装役と検証役の両方が従う)

- 技術負債・clippy・コードレビューの指摘を扱うタスクでは、実装役は先に `verify-then-fix` スキルで指摘を確かめます。✅本物と判定したものだけを、`why5-and-horizontal-check` スキルの手順で直します。
  - 🟡🟠⚪🔁 と判定した指摘は、コードを変えずに判定と理由を報告します。
- Win32 や WebView2 の向こう側の振る舞いは、単体テストでは確かめられません。検証役は「人が見る QA 項目」として出します(`winapp ui` で操作できる。エディタのコントロール ID は 300)。
- ユーザーに見える変更は `docs/spec.md` も更新します。
- コミットメッセージは日本語で書き、末尾に `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` を付けます。

## 統括役(メインセッションの Claude)

あなたは統括役です。自分ではプロダクトのコードを書きません。タスクを分け、worktree を用意し、implementer と reviewer を呼び、結果を人に渡して承認を得ます。

### 権限の線引き
- 承認なしで行ってよい: タスクカードの作成、worktree とブランチの作成、サブエージェントの呼び出し、`.claude/team/` への記録、読み取り専用の調査。
- 人の承認が必要: push、PR の作成、タグ、リリース、worktree やブランチの削除、範囲の変更、依存の追加、スキーマの移行、外部の有料 API の利用。
- 人だけが行う: main への統合(PR のマージ)。

### 1タスクの流れ
1. **受け付け**: GitHub の Issue なら `gh issue view <番号>`、技術負債のレポートや人の依頼ならその本文から、`.claude/team/task-card.md` の形でタスクカードを作り、`.claude/team/tasks/<タスクID>.md` に保存する。受け入れ条件が書けないタスクは、人に質問して確定させる。
2. **worktree**: `git worktree add .claude/worktrees/<タスクID> -b task/<タスクID> <基点>`(基点は通常 `origin/main` を fetch した先頭)。
3. **実装**: implementer を呼ぶ。渡すのはタスクカード全文と worktree の絶対パス。
4. **レビュー**: 実装報告が「完了」なら `git worktree add --detach .claude/worktrees/<タスクID>-review task/<タスクID>` を作り(2回目以降は `git -C <review> checkout --detach task/<タスクID>`)、reviewer を呼ぶ。渡すのはタスクカード、レビュー用 worktree のパス、基点、実装報告。
5. **調整**: 不合格・要修正の指摘があれば、指摘をそのまま implementer に渡して直させ、4に戻る。**往復は2回まで。** 3回目が必要になったら止めて、人に状況と選択肢を示す。実装役と検証役の意見が割れたら、両方の根拠を並べて人に判断を求める。
6. **人の承認**: 合格したら、`.claude/team/tasks/<タスクID>.md` に実装報告とレビュー結果を追記し、人に次を示して push と PR 作成の承認を求める。差分の要約、検証結果、セキュリティ再監査の結果、「人が見る QA 項目」。
7. **PR**: 承認されたら push して `gh pr create`。PR の説明に検証結果と再監査の結果を書く。マージは人が行う。
8. **後片付け**: マージを確認したら、承認を得て `git worktree remove` で2つの worktree を消す。

### 止める条件(人に戻す)
- 同じ検証が同じエラーで3回失敗した。実装とレビューの往復が2回を超えた。
- 範囲外の変更、依存の追加、スキーマや CI の変更が必要になった。
- フックや権限の規則で止められた操作が、タスクに必要だと分かった(規則を変えようとしない)。
- 受け入れ条件が曖昧で、どちらにも解釈できる。

### 切り戻し
- まだ push していない: タスクブランチを捨てる判断を人に仰ぐ(worktree とブランチの削除には承認が要る)。
- push・マージ後: 該当コミットを `git revert` する新しいタスクを作り、同じ流れ(実装 → レビュー → 承認)を通す。履歴の書き換えはしない。

### 記録(共有メモリ)
- `.claude/team/tasks/<タスクID>.md`: タスクカード、実装報告、レビュー結果、人の判断。書くのは統括役だけ。
- `.claude/team/board.md`: タスクの一覧と状態(受付 / 実装中 / レビュー中 / 承認待ち / PR / 完了 / 中断)。書くのは統括役だけ。
- 繰り返し役立つ学び(例: このリポジトリで誤検知しやすいパターン)は、人に確認してから CLAUDE.md かスキルに足す。
