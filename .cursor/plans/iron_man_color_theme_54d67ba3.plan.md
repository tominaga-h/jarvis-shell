---
name: Iron Man Color Theme
overview: プロンプトとgoodbyeメッセージにアイアンマンテーマ（Red/Yellow/Cyan/White）の配色を適用する。共通カラーモジュールを新設し、prompt.rsとbanner.rsを更新する。
todos:
  - id: create-color-module
    content: src/color.rs を新規作成し、ユーティリティ関数（red, yellow, cyan, white, bold, bold_red）を定義
    status: completed
  - id: update-main
    content: src/main.rs に mod color; を追加
    status: completed
  - id: update-prompt
    content: "src/prompt.rs を更新: crate::color を使用し、render_prompt_left に配色を埋め込み、get_prompt_color を White にオーバーライド"
    status: completed
  - id: update-banner
    content: "src/banner.rs を更新: ローカルカラー定数を crate::color に置換し、goodbye の [J.A.R.V.I.S.] を RED に変更"
    status: completed
  - id: build-verify
    content: cargo build でコンパイル確認
    status: completed
isProject: false
---

# Iron Man カラーテーマの適用

## 背景

現在、プロンプトはreedlineのデフォルト `get_prompt_color()` が `Color::Green` を返すため全体が緑一色。goodbyeメッセージの `[J.A.R.V.I.S.]` はCYANになっている。

reedlineの描画フロー（[painter.rs](../../../.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/reedline-0.44.0/src/painting/painter.rs) L357）:

```
SetForegroundColor(prompt.get_prompt_color())  // 左プロンプト全体の色を設定
Print(prompt_str_left)                          // 左プロンプトを出力
SetForegroundColor(prompt.get_indicator_color()) // インジケータの色
Print(prompt_indicator)                          // ❯ を出力
```

**方針**: `get_prompt_color()` を `Color::White` に変更し、`render_prompt_left()` にANSIカラーコードを直接埋め込むことで、パーツごとに異なる色を実現する。

## 変更対象

### 1. `src/color.rs` を新規作成 — カラーユーティリティモジュール

`to_red("text")` スタイルのユーティリティ関数でANSI着色を一元管理する。呼び出し側は `RESET` の付け忘れを気にする必要がなくなる。

```rust
// src/color.rs
/// Iron Man テーマ: Red, Yellow, Cyan, White

const RED: &str = "\x1b[91m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const WHITE: &str = "\x1b[37m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

pub fn red(text: &str) -> String { format!("{RED}{text}{RESET}") }
pub fn yellow(text: &str) -> String { format!("{YELLOW}{text}{RESET}") }
pub fn cyan(text: &str) -> String { format!("{CYAN}{text}{RESET}") }
pub fn white(text: &str) -> String { format!("{WHITE}{text}{RESET}") }
pub fn bold(text: &str) -> String { format!("{BOLD}{text}{RESET}") }
pub fn bold_red(text: &str) -> String { format!("{BOLD}{RED}{text}{RESET}") }
```

### 2. [src/prompt.rs](src/prompt.rs) を更新 — プロンプトの配色

- `use crate::color::{red, yellow, cyan, white};` を追加
- `render_prompt_left()` でユーティリティ関数を使い各パーツを着色:
  - `⚡jarvish` → `red()`
  - `in` → `white()`
  - パス (`~/dev/project`) → `yellow()`
  - `on` → `white()`
  - ブランチ名 ( `main`) → `cyan()`
- `get_prompt_color()` をオーバーライドし `Color::White`（`crossterm::style::Color`、reedlineが再エクスポート）を返す

```rust
use crate::color::{red, yellow, cyan, white};
use reedline::Color;

fn render_prompt_left(&self) -> Cow<str> {
    let cwd = env::current_dir()
        .map(|p| shorten_path(&p))
        .unwrap_or_else(|_| "?".to_string());

    let git_part = match current_git_branch() {
        Some(branch) => format!(
            " {} {}",
            white("on"),
            cyan(&format!("\u{e0a0} {branch}"))
        ),
        None => String::new(),
    };

    Cow::Owned(format!(
        "{} {} {}{git_part}\n",
        red("⚡jarvish"),
        white("in"),
        yellow(&cwd),
    ))
}

fn get_prompt_color(&self) -> Color {
    Color::White
}
```

### 3. [src/banner.rs](src/banner.rs) を更新 — goodbyeメッセージ

- ローカルのカラー定数を削除し `use crate::color::{red, cyan, bold_red, ...};` に切り替え
- welcomeメッセージ・goodbyeメッセージの着色をユーティリティ関数に置き換え
- goodbyeメッセージで `[J.A.R.V.I.S.]` を RED に変更（残りのメッセージは CYAN のまま）

変更前:

```rust
println!("  {CYAN}[J.A.R.V.I.S.] {}{RESET}", messages[idx]);
```

変更後:

```rust
println!("  {} {}", red("[J.A.R.V.I.S.]"), cyan(messages[idx]));
```

### 4. [src/main.rs](src/main.rs) を更新 — モジュール登録

`mod color;` を追加する。

## 影響範囲

- プロンプト表示部分のみ変更。既存テストには影響なし
- `banner.rs` のカラー定数が `color.rs` に移動するのみで、welcomeメッセージのレイアウトは変更なし

