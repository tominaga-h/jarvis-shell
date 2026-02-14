# /add-issue コマンド

ユーザーが入力した内容を GitHub Issue として登録する。

## 手順

1. `gh issue create -R tominaga-h/jarvis-shell --title "ユーザーが入力した内容"` を実行する
2. 作成された Issue の URL を表示する

## 例

ユーザー入力: `/add-issue ヘルプコマンドを実装する`

実行コマンド:

```
gh issue create -R tominaga-h/jarvis-shell --title "ヘルプコマンドを実装する" --body ""
```
