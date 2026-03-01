# /release コマンド

ユーザーが指定したタグをもとにリリース準備を実行する

## 注意事項

- ユーザーが指定したタグはこれから作成するものなので、存在確認は不要。

## 手順

### 0. 事前チェック

- `develop` ブランチにいることを確認する。異なるブランチの場合はユーザーに警告して確認を取る。
- `make check` を実行し、パスすることを確認する。失敗した場合はリリース手順を中断する。

### 1. リリースノートの作成

- Git コマンド（`git describe --tags --abbrev=0` など）で直前のタグを特定する
- `git log <直前のタグ>..HEAD --oneline` で差分コミットを確認する
- `docs/release` フォルダ配下に `<タグ>.md` （例: タグが `v0.1.0` の場合は `docs/release/v0.1.0.md`）を作成する
- 差分コミットの内容をもとにリリースノートを生成する
- 既存のリリースノート（`docs/release/` 内）のフォーマットに合わせること（セクション構成: 主な機能 / バグ修正 / 内部改善 / 対応プラットフォーム / 既知の制限）

### 2. チェンジログに追記

- `docs/CHANGELOG.md` に新しいバージョンのエントリを追記する
- [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) 形式に従うこと
  - セクション名: `Added` / `Changed` / `Fixed` / `Removed` 等
  - 日付フォーマット: `YYYY-MM-DD`
  - 見出し例: `## [0.2.0] - 2026-02-28`
- 既存エントリの**上**に新しいバージョンを追加する（最新が上）

### 3. バージョンの変更

以下の既存ドキュメント等に記載されたバージョンを、指定されたタグに変更する。

- `README.md` および `docs/README_JA.md`
  - versionバッヂの値、リンク先のリリースURL（タグそのまま。例: `v0.2.0`）
  - curlコマンドのアセットURL（タグそのまま。例: `v0.2.0`）
- `Cargo.toml`
  - バージョンの値（`v` prefix を除いた値を設定する。例: タグが `v0.2.0` なら `"0.2.0"`）

### 4. コミット・マージ・タグ打ち・プッシュ・GitHub Release

事前にユーザーに以下の操作を自動実行してよいか確認すること。
確認してユーザーがNGを返したら、**フォールバック手順**（後述）を提示して処理を一時停止すること。

#### 自動実行する操作

1. **develop でのコミット**
   - 変更したファイルを `git add` する
   - `git commit -m "Release <タグ>"` でコミットする
2. **main ブランチへマージしてタグを打つ**
   - `git checkout main`
   - `git merge --no-ff develop -m "Merge branch 'develop' for release <タグ>"`
   - `git tag <タグ>`
3. **リモートへのプッシュ**
   - `git push origin main --tags`
   - `git checkout develop`（作業ブランチに戻る）
   - `git push origin develop`
4. **GitHub Release の作成**
   - `gh release create <タグ> --notes-file docs/release/<タグ>.md`
5. **リリースビルド**
   - `make release`

#### フォールバック手順（ユーザーが NG を返した場合）

以下のように実行すべきコマンド一覧を `<変更ファイル>` と `<タグ>` を実際の値に置換した状態で表示し、処理を一時停止する。

```bash
以下のコマンドを手動で実行してください:

# develop でコミット
git add <変更ファイル一覧>
git commit -m "Release <タグ>"

# main へマージしてタグ打ち
git checkout main
git merge develop
git tag <タグ>

# プッシュと作業ブランチへの復帰
git push origin main
git push origin <タグ>
git checkout develop
git push origin develop

# GitHub Release を作成
gh release create <タグ> --notes-file docs/release/<タグ>.md

# リリースビルド
make release
```
