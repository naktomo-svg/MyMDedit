# プレビューの攻撃用ペイロード集

プレビューのセキュリティ（CSP・`IsScriptEnabled = false`・`preview.rs` のハンドラー）の動的テスト用。
外部への URL はすべて `*.mymdedit-canary.invalid`（名前解決できないホスト）を指す。通信が起きたかどうかは、
WebView2 の netlog（`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--log-net-log=…`）に、
このホスト名が現れるかで判定する。SMB・WebDAV の試行は、どこにも届かない 192.0.2.0/24（TEST-NET-1）への
TCP 接続として観測する。

## まとめて流す

```powershell
cargo build
.\tests\security\tools\run-all.ps1            # 全ペイロード（約 5 分）
.\tests\security\tools\run-all.ps1 -Only unc-leak,nav-click
```

ペイロードごとに新しい APPDATA/LOCALAPPDATA でアプリを起動し（ユーザーの設定と WebView2 のプロファイルには
触れない）、クリックしてから、netlog・リスナー・TCP 表・stderr・ダウンロードフォルダ・プロファイルを照合して
PASS/FAIL を出す。陽性対照（https の画像）が観測できないときは「observation broken」として FAIL にする。
マウスでクリックできない（画面のロック中など）ときは UIA Invoke でクリックし、結果にそう書き添える。

| ファイル | 操作 | 期待する結果 |
|---|---|---|
| `p2-passive.md` | 開くだけ | `ctrl-img` だけがリクエストまで進む（陽性対照） |
| `f1-download-data.md` | リンクをクリック | ファイルが保存されない |
| `f1-download-https.md` | リンクをクリック | 通信もファイル保存もない |
| `f3-iframe.md` | iframe 内のリンクをクリック | iframe は文字として表示され、通信もない |
| `nav-click.md` | 各リンクをクリック | ガードが止める（`PreviewNavigationBlocked`）、外部アプリが起動しない |
| `f1-download-txt.md` | リンクをクリック | ファイルが保存されない |
| `local-leak.md` | 開く・リンクをクリック | リスナーに平文のリクエストが届かない（陽性対照の 18440 を除く） |
| `unc-leak.md` | 開く・リンクをクリック | 192.0.2.x へは陽性対照（`192.0.2.25:443`）以外の接続がない（SMB・WebDAV の試行なし） |
| `copy-link.md` | 偽造したコピーボタンのリンクをクリック | どのコードブロックもコピーされず（`PreviewCopyIgnored`）、`target="_blank"` は新しいウィンドウにならない。通信もない |

全ペイロードで、プロファイルに `data:text/html`（文書そのもの）が残らないことも確かめる（F16）。

## 自動化していない確認（手作業）

- F2：プレビューで F12・Ctrl+Shift+I・右クリックをしても、DevTools や既定のコンテキストメニューが出ない。
- F13：クライアント証明書を要求するサーバーの画像を置いても、証明書の選択ダイアログが出ない（相互 TLS のサーバーが必要）。

## ツール

- `tools/run-all.ps1 [-Only 名前,…]`：上の表を全部流す。
- `tools/run-payload.ps1 <名前>`：1 つだけ起動する（netlog と stderr を `%TEMP%\mymdedit-security-out` に出す）。
- `tools/check_netlog.py <netlog> [--allow ホスト] [--require ホスト]`：netlog の合否判定。
- `tools/netlog_hosts.py <netlog>`：netlog の中身をホストごとにまとめる（調査用）。
- `tools/listener.py <出力ファイル> <秒数>`：127.0.0.1:18440-18459 で TCP を待ち受ける。
- `tools/tcpwatch.ps1 <出力ファイル> <秒数>`：192.0.2.0/24 への TCP 接続を記録する（管理者権限は不要）。

Python は `py -I` で実行する。
