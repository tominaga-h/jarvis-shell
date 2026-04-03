---
description: GitHub Issue の一覧をマイルストーン付きで表示する
argument-hint: "[closed|all]"
---

GitHub Issue の一覧をマイルストーン付きで表示する。

引数: `$ARGUMENTS` (省略可。`closed` または `all` を指定できる)

## 手順

1. 引数に応じてステートを決定する:
   - 引数なし: `--state open`
   - `closed`: `--state closed`
   - `all`: `--state all`

2. 以下のコマンドで Issue を JSON 取得する:
   ```
   gh issue list -R tominaga-h/jarvis-shell --state <state> --json number,title,labels,state,milestone
   ```

3. 結果を以下のテーブル形式で表示する（マイルストーンなしは `—`）:

   | # | タイトル | ラベル | マイルストーン |
   |---|---|---|---|
   | 89 | completion機能 | Shell function | v2.0.0 |
   | 88 | git補完の拡充 | — | — |
