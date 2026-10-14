# local leak

ポートごとに経路が違う（listener.py が 127.0.0.1:18440-18459 で接続を記録する）。

<img src="https://127.0.0.1:18440/ctrl-img-https">
<img src="http://127.0.0.1:18441/img-http">
<link rel="prefetch" href="https://127.0.0.1:18442/prefetch-https">
<link rel="prefetch" href="http://127.0.0.1:18443/prefetch-http">
<link rel="preconnect" href="https://127.0.0.1:18444/">
<iframe src="http://127.0.0.1:18445/iframe-http"></iframe>
<meta http-equiv="refresh" content="2;url=http://127.0.0.1:18446/refresh-http">

<a href="http://127.0.0.1:18447/click-http">CLICKHTTP</a>

<a href="https://127.0.0.1:18448/click-https">CLICKHTTPS</a>
