# プレビューの攻撃用ペイロード集

`docs/security/2026-10-14-audit-plan.md` の動的テスト用。外部への URL はすべて
`*.mymdedit-canary.invalid`（名前解決できないホスト）を指す。通信が起きたかどうかは、
WebView2 の netlog（`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--log-net-log=…`）に、
このホスト名が現れるかで判定する。手順と結果は `docs/security/2026-10-14-dynamic-p1.md`。

| ファイル | 操作 | 期待する結果 |
|---|---|---|
| `p2-passive.md` | 開くだけ | `ctrl-img` だけが netlog に出る（陽性対照） |
| `f1-download-data.md` | リンクをクリック | ファイルが保存されない |
| `f1-download-https.md` | リンクをクリック | 通信もファイル保存もない |
| `f3-iframe.md` | iframe 内のリンクをクリック | iframe 内で遷移しない、通信もない |
| `nav-click.md` | 各リンクをクリック | ガードが止める（`PreviewNavigationBlocked`） |
| `f1-download-txt.md` | リンクをクリック | ファイルが保存されない |
| `local-leak.md` | 開く・リンクをクリック（`tools/listener.py` を先に起動） | リスナーに何も届かない（陽性対照の 18440 を除く） |

ツール：`tools/run-payload.ps1 <名前>`（起動）、`tools/netlog_hosts.py <netlog>`（解析）、
`tools/listener.py <出力ファイル> <秒数>`（127.0.0.1 で TCP を待ち受ける）。Python は `py -I` で実行する。
