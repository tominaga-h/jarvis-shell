/// zsh ブリッジ本体（`assets/zsh/capture.zsh` を vendor したもの）。
pub(crate) const CAPTURE_SCRIPT: &str = include_str!("../../../../assets/zsh/capture.zsh");

/// ブリッジ用 `.zshrc` の初回生成テンプレート。
///
/// fpath 追加や `compdef` の書き方をコメントで示す最小限のサンプル。
/// ユーザーが自由に書き換えてよい（jarvish は既存ファイルを上書きしない —
/// [`ensure_bridge_zshrc`] 参照）。
pub(super) const BRIDGE_ZSHRC_TEMPLATE: &str = r#"# jarvish zsh completion bridge — ~/.config/jarvish/zsh-bridge/.zshrc
#
# このファイルは jarvish の Tab 補完が内部で起動する "ブリッジ用" zsh
# だけが読み込みます。あなたの通常の ~/.zshrc には一切影響しません。
# This file is sourced only by the internal zsh jarvish spawns for Tab
# completion. It has no effect on your normal ~/.zshrc.
#
# ここに書いた fpath 追加や compdef は、本物の zsh 構文でそのまま使えます。
# Anything you write here (fpath additions, compdef, zstyle, ...) uses real
# zsh syntax — no jarvish-specific DSL to learn.

# 例1: Homebrew でインストールした zsh-completions を fpath に追加する
# Example: add Homebrew's zsh-completions to fpath
#   brew install zsh-completions
# fpath=(/opt/homebrew/share/zsh-completions $fpath)
#
# 注意: 追加するディレクトリやその親ディレクトリが group-writable だと、
# zsh の compinit セキュリティ検査（compaudit）に引っかかり補完が全滅
# することがあります（例: Intel Mac の /usr/local/share）。`compaudit`
# で確認し、必要なら `chmod g-w /usr/local/share` を実行してください。
# Warning: if the directory you add (or its parent) is group-writable,
# zsh's compinit security check (compaudit) may flag it and silently
# break all completions (e.g. /usr/local/share on Intel Macs). Check
# with `compaudit`, and if needed run `chmod g-w /usr/local/share`.

# 例2: 自作/追加の補完関数を任意のディレクトリから読み込む
# Example: load custom completion functions from your own directory
# fpath=(~/.zsh/completions $fpath)

# 例3: 特定コマンドに補完関数を明示的に紐付ける (compdef)
# Example: bind a completion function to a command explicitly
# compdef _git my-git-wrapper
"#;
