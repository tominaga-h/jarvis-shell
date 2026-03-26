指定したタグをもとにリリース準備を実行する。

引数: `$ARGUMENTS` (タグ名。例: `v1.6.0`)

## 注意事項

- 引数で指定したタグはこれから作成するものなので、存在確認は不要。

## 手順

### 0. 事前チェック

- `develop` ブランチにいることを確認する。異なるブランチの場合はユーザーに警告して確認を取る
- `make check` を実行し、パスすることを確認する。失敗した場合はリリース手順を中断する

### 1. リリースノートの作成

- `git describe --tags --abbrev=0` で直前のタグを特定する
- `git log <直前のタグ>..HEAD --oneline` で差分コミットを確認する
- `docs/release/<タグ>.md` を作成する（例: `v1.6.0` → `docs/release/v1.6.0.md`）
- 差分コミットの内容をもとにリリースノートを生成する
- 既存のリリースノート（`docs/release/` 内）のフォーマットに合わせる（セクション構成: 主な機能 / バグ修正 / 内部改善 / 対応プラットフォーム / 既知の制限）

### 2. チェンジログに追記

- `docs/CHANGELOG.md` に新しいバージョンのエントリを追記する
- [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) 形式に従う
  - セクション名: `Added` / `Changed` / `Fixed` / `Removed` 等
  - 日付フォーマット: `YYYY-MM-DD`
  - 見出し例: `## [1.6.0] - 2026-04-01`
- 既存エントリの**上**に新しいバージョンを追加する（最新が上）

### 3. バージョンの変更

以下のファイルに記載されたバージョンを指定タグに更新する:

- `README.md` および `docs/README_JA.md`
  - version バッジの値、リンク先のリリース URL（タグそのまま。例: `v1.6.0`）
- `Cargo.toml`
  - `version` の値（`v` prefix を除く。例: タグが `v1.6.0` なら `"1.6.0"`）

### 4. コミット・マージ・タグ打ち・プッシュ・GitHub Release

事前にユーザーに以下の操作を自動実行してよいか確認すること。
ユーザーが NG を返した場合は**フォールバック手順**（後述）を提示して処理を一時停止すること。

#### 自動実行する操作

1. **develop でのコミット**
   ```bash
   git add <変更ファイル一覧>
   git commit -m "Release <タグ>"
   ```
2. **main ブランチへマージしてタグを打つ**
   ```bash
   git checkout main
   git fetch origin main
   git merge --ff FETCH_HEAD   # 必要な場合のみ
   git merge --no-ff develop -m "Merge branch 'develop' for release <タグ>"
   git tag <タグ>
   ```
3. **リモートへのプッシュ**
   ```bash
   git push origin main --tags
   git checkout develop
   git push origin develop
   ```
4. **GitHub Release の作成**
   ```bash
   gh release create <タグ> --notes-file docs/release/<タグ>.md
   ```
5. **リリースビルド**
   ```bash
   make release
   gh release upload <タグ> ./target/release/jarvish
   cargo publish
   ```

#### フォールバック手順（ユーザーが NG を返した場合）

以下のコマンドを `<変更ファイル>` と `<タグ>` を実際の値に置換した状態で表示し、処理を一時停止する:

```bash
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
gh release upload <タグ> ./target/release/jarvish
cargo publish
```
