# /issues コマンド

GitHub Issue の一覧を表示する。

## 手順

1. `gh issue list -R tominaga-h/jarvis-shell` を実行し、オープン中の Issue 一覧を表示する
2. ユーザーが `closed` や `all` を指定した場合は `--state` オプションを付与する

## 例

ユーザー入力: `/issues`

実行コマンド:

```
gh issue list -R tominaga-h/jarvis-shell
```

ユーザー入力: `/issues all`

実行コマンド:

```
gh issue list -R tominaga-h/jarvis-shell --state all
```

ユーザー入力: `/issues closed`

実行コマンド:

```
gh issue list -R tominaga-h/jarvis-shell --state closed
```
