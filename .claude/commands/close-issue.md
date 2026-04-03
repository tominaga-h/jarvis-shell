---
description: 指定した番号の GitHub Issue をクローズする
argument-hint: "<issue-number>"
---

指定した番号の GitHub Issue をクローズする。

## 手順

1. 引数 `$ARGUMENTS` を Issue 番号 N として受け取る
2. 以下のコマンドを実行する:
   ```
   gh issue close N -R tominaga-h/jarvis-shell
   ```
3. クローズ結果を表示する
