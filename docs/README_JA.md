# 🤵 Jarvish — The AI-Native Shell

[![status](https://img.shields.io/github/actions/workflow/status/tominaga-h/jarvis-shell/ci.yml)](https://github.com/tominaga-h/jarvis-shell/actions)
[![version](https://img.shields.io/badge/version-1.15.0-blue)](https://github.com/tominaga-h/jarvis-shell/releases/tag/v1.15.0)

> 🌐 [English README](../README.md)

## 💡 概要

> _「アイアンマンの J.A.R.V.I.S. のような相棒が欲しい。ただし、ターミナルの中で。」_

**Jarvish** は、Marvel の Iron Man に登場する **J.A.R.V.I.S.** にインスパイアされた、Rust 製の **次世代 AI 統合シェル (Next Generation AI Integrated Shell)** です。

既存のシェル（BashやZsh）の単なるラッパーや外部ツールではありません。ターミナルのワークフローそのものにAIを深く統合し、**「通常のコマンド」と「自然言語」を息をするようにシームレスに行き来できる**これまでにない体験を提供します。

エラーをブラウザにコピペしてAIに聞く時代は終わりました。Jarvish に聞くだけです。

[![jarvish-demo](../images/jarvish-demo.gif)](https://asciinema.org/a/806755)

## 📑 目次

- [概要](#-概要)
- [コア・エクスペリエンス](#-コアエクスペリエンス)
  - [ターミナルに住む、あなたの専属アシスタント](#1-ターミナルに住むあなたの専属アシスタント)
  - [AIパイプ ＆ AIリダイレクト](#2-aiパイプ--aiリダイレクト最強のテキスト処理)
  - ["The Black Box"](#3-the-black-box完全記憶型ストレージ)
  - [妥協のない「爆速」シェル UX](#4-妥協のない爆速シェル-ux)
- [インストール](#-インストール)
- [アップデート](#-アップデート)
- [セットアップと設定](#️-セットアップと設定)
  - [Starship プロンプト連携](#starship-プロンプト連携)
  - [外部補完連携 (carapace)](#外部補完連携-carapace)
  - [zsh 補完ブリッジ](#zsh-補完ブリッジ)
  - [カスタム補完（`complete` ビルトイン）](#カスタム補完complete-ビルトイン)
  - [起動スクリプト（`rc.jsh`）](#-起動スクリプトrcjsh)
- [アーキテクチャ](#️-アーキテクチャ)
- [開発への参加](#-開発への参加)

## ✨ コア・エクスペリエンス

### 1. ターミナルに住む、あなたの専属アシスタント

- **自然言語による直接実行**: プロンプトから日本語で「今動いてるポート一覧を見せて」と打つだけで、最適なコマンドに翻訳して実行します。
- **スマートエラーハンドリング**: コマンドが失敗すると、Jarvish が直前の `stdout`/`stderr` のコンテキストを読み取り、自動的に原因を分析・解決案を提示します。
- **自律的なエージェント機能**: 単なるチャットではなく、Jarvish 自身がファイルの読み書きやコマンドの再実行を行うことができます（Tool Calls）。

### 2. AIパイプ ＆ AIリダイレクト（最強のテキスト処理）

`awk` や `sed`、`jq` の複雑なシンタックスを思い出す必要はもうありません。

- **AI パイプ (`| ai "..."`)**: コマンドの出力を自然言語で直接フィルタリング・整形します。
  ```bash
  ls -la | ai "一番重いファイルは？"
  docker ps | ai "コンテナIDとイメージ名をJSONで出力して"
  ```
- **AI リダイレクト (`> ai "..."`)**: コマンドの出力をJarvishのコンテキストに送り、対話的な分析を依頼します。
  ```bash
  git log --oneline -10 > ai "最近のコミットの変更意図を要約して"
  eza --help > ai "--treeオプションに追加で指定できるオプションは？"
  ```

### 3. "The Black Box"（完全記憶型ストレージ）

Jarvish はターミナルで起きたすべての出来事を記憶しています。

- **Gitライクな履歴保存**: 実行したコマンド、タイムスタンプ、ディレクトリ、終了コード、そして `stdout`/`stderr` の全出力結果を、コンテンツアドレッサブルなBlobストレージ（SHA-256 + zstd 圧縮）に永続化します。
- **時間を遡るコンテキスト**: シェルを再起動しても、「昨日発生したあのエラーの原因は何だっけ？」とJarvishに質問できます。
- **セキュリティ**: `.bashrc` などに含まれる可能性のある APIキー や トークン などの機密情報は、保存時に自動で **マスキング** される安全設計です。

### 4. 妥協のない「爆速」シェル UX

AIを統合しながらも、Rustの特性を活かし、インフラツールとしての圧倒的なパフォーマンスを誇ります。

- **非同期バックグラウンド・プロンプト**: Gitのステータススキャンを別スレッドで処理し（Stale-While-Revalidate パターン採用）、どれだけ巨大なリポジトリでもタイピングの遅延（UIジッター）を**完全にゼロ**にしました。
- **Fishライクなオートコンプリート**: リアルタイムなシンタックスハイライトと、PATHバイナリやファイルパスの強力な自動補完機能を備えています。さらに [carapace](#外部補完連携-carapace) 連携により、数百種類の CLI ツールの引数・フラグ補完にも対応します（任意）。
- **完全な PTY サポート**: `vim` や `top` などの対話型プログラムもネイティブに動作します。
- **ジョブ制御による Ctrl+C**: コマンド実行中に `Ctrl+C` を押すと、実行中のコマンドだけが中断され、Jarvish シェル本体は終了しません。外部コマンドは独立したプロセスグループで起動され、端末のフォアグラウンドを一時的に委譲されるため、端末が生成する `SIGINT` は子プロセスグループにのみ届きます。
- **Starship 連携**: [Starship](https://starship.rs/) プロンプトをネイティブサポート。既存の Starship 設定をそのまま利用できます。
- **グロブ展開とブレース展開**: bash/zsh 互換のファイル名展開:
  - グロブ: `ls *.toml`, `cat Cargo.???`, `rm [Cc]argo.lock`
  - ブレース: `echo {a,b,c}`, `echo {1..5}`, `mkdir -p src/{api,cli}/v{1..3}`
  - 組み合わせ: `cp *.{txt,md} backup/`
  - zsh 互換: マッチなしはエラー終了（`jarvish: no matches found: <pattern>`）
  - クォート/エスケープを尊重: `'*'`, `"{a,b}"`, `\*` はリテラル扱い
- **`cdhist` / `cdj` ディレクトリジャンプ**: 過去に訪問したディレクトリへシェル内で即復帰:
  - `cdhist [--limit N]` — 訪問履歴を LRU 順で 1 行 1 件出力（重複排除、現在の cwd は除外）
  - `cdj [pattern]` — `fzf` 経由でファジー選択して `cd`（`fzf` を `PATH` に要する）。`pattern` で case-insensitive substring 絞り込み、単一マッチなら fzf を起動せず即 cd。fzf プレビューに選択中ディレクトリの `ls -Cp` を表示（UNIX のみ）
  - データソースは既存 `command_history.cwd`、新規スキーマなし

## 🚀 インストール

### 前提条件

- **OpenAI API キー**
- **NerdFont** (プロンプトのアイコン表示に推奨)

### Homebrew でインストール (macOS)

```bash
brew tap tominaga-h/tap
brew install tominaga-h/tap/jarvish
```

### Cargo でインストール

```bash
cargo install jarvish
```

### ソースからビルド

```bash
git clone https://github.com/tominaga-h/jarvis-shell.git
cd jarvis-shell
cargo install --path .
```

## 🔄 アップデート

Jarvish にはビルトインの `update` コマンドがあり、最新バージョンへの自己更新が可能です。

```bash
update            # GitHub Releases から最新バージョンに更新
update --check    # 新しいバージョンがあるか確認（インストールはしない）
```

Homebrew でインストールされている場合は自動検知し、`brew upgrade jarvish` の使用を案内します。

### ローカルバイナリからのアップデート

ソースからビルドする開発者向けに、ローカルのビルド済みバイナリからの更新もサポートしています：

```bash
update --local                    # デフォルトパス（target/release/jarvish）を使用
update --local /path/to/jarvish   # カスタムパスのバイナリを使用
update --check --local            # ローカルバイナリのバージョンを確認（インストールはしない）
```

更新が成功すると、jarvish は新しいバージョンを適用するために自動的に再起動します。

## ⚙️ セットアップと設定

OpenAI API キーを環境変数に設定してください：

```bash
export OPENAI_API_KEY="sk-..."
```

> ※ `~/.config/jarvish/config.toml` の `[export]` セクションに記述することで自動設定も可能です。

### 設定ファイル (`config.toml`)

初回起動時に `~/.config/jarvish/config.toml` にデフォルト設定が自動生成されます。

```toml
[ai]
model = "gpt-4o"              # 使用する AI モデル
max_rounds = 10               # エージェントの自律ループ最大回数
markdown_rendering = true     # AIの回答をMarkdownで綺麗に表示
ai_pipe_max_chars = 50000     # AIパイプへの入力文字数上限（超過時は安全にFail-fast）
ai_redirect_max_chars = 50000 # AIリダイレクトへの入力文字数上限（超過時は安全にFail-fast）
temperature = 0.5             # 回答のランダム性
ignore_auto_investigation_cmds = ["git log", "git diff"]  # 自動調査をスキップするコマンド

[alias]
g = "git"                     # コマンドエイリアス（ビルトインでも管理可）
ll = "eza --icons -la"

[export]
PATH = "/usr/local/bin:$PATH" # 起動時に展開される環境変数
# ⚠️ SHELL = "/usr/local/bin/jarvish" の設定に注意:
# 外部ツール（Cursor, VS Code 等）がサブシェルとして jarvish を使用するようになり、
# ツール呼び出しフックの失敗が AI 自動調査を大量発火させる可能性があります。
# 対話的シェルとしてのみ jarvish を使用する場合は SHELL を bash/zsh のままにしてください。

[prompt]
nerd_font = true              # NerdFont 未インストールの場合は false に設定
starship = false              # true にすると Starship プロンプトを使用（要: starship コマンド + ~/.config/starship.toml）

[completion]
git_branch_commands = ["checkout", "switch", "merge", "rebase", "branch", "diff", "log", "cherry-pick", "reset", "push", "fetch"]
external = "auto"             # "auto" | "carapace" | "zsh" | "none" | ["carapace", "zsh"] — 外部補完の使用方針（文字列 or 配列）
external_timeout_ms = 400     # 外部補完プロセスのタイムアウト（ミリ秒）
external_zsh_daemon = true    # zsh ブリッジを常駐デーモン化するか（下記「zsh 補完ブリッジ」参照）

[startup]
commands = [                      # シェル起動時に順次実行するコマンド（-c オプション実行時はスキップ）
    "echo 'Welcome to jarvish!'",
    "export JAVA_HOME=/usr/lib/jvm/default",
]
```

> **ヒント**: 設定を変更した後は、`source` コマンドで再起動せずに適用できます。
>
> ```bash
> source ~/.config/jarvish/config.toml
> ```

### Starship プロンプト連携

Jarvish は [Starship](https://starship.rs/) をプロンプトのカスタマイズ手段としてネイティブにサポートしています。有効化すると、初期化スクリプトなしで `starship prompt` を直接呼び出します。

**前提条件:**

1. `starship` コマンドが PATH 上にインストールされていること
2. `~/.config/starship.toml`（または `STARSHIP_CONFIG` 環境変数で指定したパス）が存在すること

**設定方法:**

```toml
# ~/.config/jarvish/config.toml
[prompt]
starship = true
```

Jarvish は `starship prompt` に `--status`、`--cmd-duration`、`--terminal-width` を渡すため、`character`、`cmd_duration`、`status` などの Starship モジュールが正しく動作します。

`starship = true` を設定しているが前提条件を満たしていない場合は、警告を表示してビルトインプロンプトにフォールバックします。

### 外部補完連携 (carapace)

Jarvish の Tab 補完は [carapace](https://github.com/carapace-sh/carapace-bin) と連携できます。carapace は git・docker・kubectl など 500 以上の CLI ツールの補完を提供するマルチシェル対応の補完エンジンです。`brew install carapace` でインストールできます。

- **`[completion] external` は文字列と配列の両方を受け付けます**:
  - `"auto"`（デフォルト）は各プロバイダを優先順（carapace → [zsh ブリッジ](#zsh-補完ブリッジ)）で試し、バイナリが見つかったものだけを有効化します。追加設定は不要です。
  - `"none"` は外部補完を完全に無効化します。
  - `"carapace"` / `"zsh"` はそのプロバイダのみを有効化します（バイナリ未検出時は警告を表示）。
  - `["zsh", "carapace"]` のような配列を指定すると、その記載順を優先順として明示指定できます。各要素は左から順に試され、バイナリが見つかったものだけが有効化されます。不正な要素は警告のうえスキップされ、残りの要素は引き続き適用されます。
- **タイムアウト + フォールバック**: 各外部補完呼び出しは `external_timeout_ms`（デフォルト 400ms）でタイムアウトします。あるプロバイダがハング・エラー・候補なしを返した場合、Jarvish は自動的に次のプロバイダ（最終的にはビルトインのパス補完）へフォールバックします — Tab キーが外部プロセス待ちでブロックされることはありません。
- **ホットリロード**: `external` と `external_timeout_ms` は `source` ビルトインで再読み込みされ、そのたびに設定済みの各プロバイダのバイナリの再検出（`which`）も行われます。つまりセッション中に `brew install carapace` した後、`source ~/.config/jarvish/config.toml` を実行するだけで、再起動なしに即座に有効化できます（注意: 配列の**並び順**の変更 — 例えば `["carapace", "zsh"]` を `["zsh", "carapace"]` に入れ替える — は次回の Jarvish 起動まで反映されません。プロバイダの有効/無効化とバイナリの再検出は即座に反映されます）。
- **カバレッジの拡大**: carapace は実際のシェル補完関数（zsh の `compsys` など）へのブリッジもサポートしています。`config.toml` の `[export]` セクションで `CARAPACE_BRIDGES`（例: `CARAPACE_BRIDGES = "zsh"`）を設定すると、carapace が標準搭載していない補完も取り込めます。

### zsh 補完ブリッジ

[carapace](#外部補完連携-carapace) が対象コマンドの候補を持っていない（または有効化されていない）場合、Jarvish はビルトインの zsh ブリッジにフォールバックします。バックグラウンドで実際の zsh を起動し、その本物の補完システム（`compsys`、`_*` 補完関数群）に候補を尋ねます。つまり、zsh 上で動く補完関数であれば（サードパーティ製のものも含めて）、carapace の対応有無に関わらず Jarvish でも使えます。上記と同じ `[completion] external` 設定で制御します（例: `external = "zsh"` で zsh ブリッジのみを使用、`external = ["zsh", "carapace"]` で carapace より優先）。

- **ブリッジ用 zshrc**: ブリッジ zsh は、あなたの実 `~/.zshrc` ではなく `~/.config/jarvish/zsh-bridge/.zshrc` を読み込みます。そのため対話シェルの設定から隔離されています。このファイルが存在しない場合、ブリッジ初回実行時にコメント付きテンプレートとして Jarvish が自動生成します — 以後は一切上書きされないため、自分で加えた変更は安全です。
- **補完の追加方法**: ブリッジ用 zshrc には普通の zsh 構文がそのまま書けます。例えば Homebrew でインストールした [`zsh-completions`](https://github.com/zsh-users/zsh-completions) を取り込むには:
  ```sh
  # ~/.config/jarvish/zsh-bridge/.zshrc
  fpath=(/opt/homebrew/share/zsh-completions $fpath)
  ```
  通常の `~/.zshrc` と同じように `compdef` 行を追加して、特定コマンドに補完関数を紐付けることもできます。
- **タイムアウト + フォールバック**: carapace と同様、各ブリッジ呼び出しはタイムアウトで保護されています（`external_timeout_ms` と共有しつつ、zsh の `compinit` 起動コストを見込んだ下限値を設けています）。ブリッジがハング・エラー・候補なしを返した場合は、ビルトインのパス補完へフォールバックします — Tab キーが UI をブロックすることはありません。
- **常駐デーモン（`external_zsh_daemon`）**: ワンショット方式（Tab のたびに新しい zsh を起動し、`compinit` を走らせ、補完して終了する）は、実測でおよそ 700〜1100ms かかります — その大半はプロセス/PTY の起動コストで、補完の計算自体ではありません。`external_zsh_daemon = true`（既定）のとき、Jarvish は `zsh -i` を **Jarvish の素の子プロセスとして**1本だけ spawn し、以後の Tab 押下ではそれを使い回します。これはシステムサービスではなく、`launchd`/`launchctl` も一切使いません — Jarvish シェルが生きている間だけ存在する per-session の子プロセスです。このデーモンは**シェル起動直後にバックグラウンドで事前ウォームアップ**されるため、通常は最初の Tab 押下の時点で既にウォーム状態になっています。プリウォームがまだ完了していない（または zsh が見つからない等の理由でスキップされた）場合は、代わりに最初に必要になった Tab 押下で遅延 spawn されます。ウォーム状態になった後、リクエストは補完の計算コストのみを払います（目安として数ミリ秒）。`tmuxinator` の Ruby 製補完のように、遅いインタプリタを起動する補完関数も許容されます — ウォームリクエストのタイムアウトは 2000ms を下限とし、1回のタイムアウト（=遅い補完）だけではデーモンを kill しません。遅れて届いた応答は次の Tab 押下時に読み飛ばして破棄する（drain）だけに留めます。**連続2回**タイムアウトした場合のみ本当にハングしたと判定し、デーモンをバックグラウンドで kill したうえで、次の Tab 押下で新しいデーモンが遅延 spawn されます。ブリッジ用 zshrc（下記参照）を編集すると、そのファイルの更新時刻の変化を自動検知して次の Tab 押下で透過的にデーモンを再起動するため、`fpath`/`compdef` を書き換えた後に Jarvish 自体を再起動する必要はありません。`external_zsh_daemon = false` にすると常にワンショット方式を使います（ブリッジのトラブルシューティング時の手動エスケープハッチとしても使えます）。`source` によるホットリロードに対応しており、off にすると稼働中のデーモンは**その `source` 実行時点で**即座に shutdown され、on に戻すと次回の zsh 補完リクエストで遅延 spawn されます。稼働中のデーモンは Jarvish の終了時・再起動時（`restart` ビルトイン経由を含む）にも必ず明示的に shutdown され、Jarvish のセッションより長生きすることはありません。

**トラブルシューティング: `fpath` を編集したらブリッジ補完が全コマンドで何も返さなくなった。** 上記の例のようにブリッジ用 zshrc の `fpath` にディレクトリを追加した結果、ブリッジ補完が*すべての*コマンドで候補を返さなくなった場合、原因はほぼ確実に zsh の `compinit` セキュリティ検査です。`compinit` は内部で `compaudit` を実行しますが、これは `fpath` に追加したディレクトリだけでなく、その**親ディレクトリ**も検査対象にします。いずれかが group-writable だと安全でないと判定され、`Ignore insecure directories and continue [ny]?` という対話的プロンプトを表示します。ブリッジ用の zsh は不可視の `zpty` セッション内で動いているため、このプロンプトに誰も答えられず `compinit` がハングし、補完が全滅したように見えます。これは Intel Mac で特によく起こります（Homebrew の `/usr/local/share` が既定で group-writable なため）。Apple Silicon の `/opt/homebrew` ではこの問題はほとんど発生しません。`compaudit` コマンドで該当ディレクトリを確認し、Homebrew 公式が推奨するのと同じ対処 `chmod g-w /usr/local/share` を行ってください。

### カスタム補完（`complete` ビルトイン）

carapace や zsh ブリッジがカバーしていないコマンド（自作スクリプトや社内 CLI など）向けに、Jarvish はプロンプト上で直接補完を定義できる fish 風の `complete` ビルトインを提供します。外部ツールは不要です。

- **登録**: `complete -c CMD [-s X]... [-l LONG]... [-a 'WORDS'] [-d DESC] [-n COND]` で `CMD` に 1 個の補完仕様を追加します。`-c/--command` は必須です。`-s` は単一文字のショートフラグ（例: `-s v` で `-v`）を指定します — 単一の ASCII 印字可能文字である必要があり、クォート文字（`'`/`"`）やバックスラッシュは使えません。`-l/--long-option` はロングフラグ名（例: `-l verbose` で `--verbose`）を指定します — 空文字列不可、空白やバックスラッシュも使えません。いずれも複数回指定して 1 回の呼び出しで複数フラグを登録したり、同じコマンドに対して `complete` を繰り返し呼んで蓄積させたりできます。`-a/--arguments` は空白区切り（クォート可）の静的な候補語リスト、または単一の動的ソース `"$(コマンド)"`（下記参照）のいずれかを指定します。`-d/--description` は補完メニューに表示されるフォールバック用の説明文を設定します。`-n/--condition` はサポート対象の組み込み条件（下記参照）にのみ絞り込みます。未対応の条件式を持つ spec も登録・一覧表示はされますが、補完候補は一切出しません。`-c`・`-a`・`-d`・`-n` の値に改行・キャリッジリターン・NUL バイトを含めることはできません — `complete` の round-trip 可能な一覧表示を破壊するため、登録時点で拒否されます（終了コード 2）。
- **一覧**: 引数なしの `complete` は、登録済みの全 spec を、登録時と同じ `complete -c ...` 構文で 1 行 1 spec ずつ出力します。Jarvish 自身のシェルパーサで再トークナイズすれば出力をそのまま再実行できます（round-trip 可能）。英数字と `_ . / : = + , @ % ^ -` からなる保守的な安全文字集合以外を 1 文字でも含む値は自動的にシングルクォートで囲まれます（バックスラッシュを含む値も対象 — 再パース時にエスケープとして解釈されるのを防ぐため）。
- **消去**: `complete -e -c CMD` で `CMD` に登録済みの全 spec を消去します。`-c` なしの `-e` はエラーになります。
- **単独コマンドとしてのみ有効**: `complete` は単独の単純コマンドとして実行する必要があります（`;`、`&&`、`||`、パイプを含めない）。例: 1 行で `complete -c foo -a bar` を実行するか、`jarvish -c "complete -c foo -a bar"` として渡します。パイプライン内、`cmd1 ; complete ...` のようなコマンドリスト内、`ai` へのパイプ内で実行した場合はシェルが保持する実レジストリにアクセスできず、登録内容を黙って破棄するのではなく `complete: can only be used as a standalone command`（終了コード 1）というエラーになります。`complete --help` はどの経路からでも動作します。

例:

```sh
complete -c mycmd -s v -l verbose -d 'Verbose output'
complete -c mycmd -a 'start stop restart' -d 'Subcommand'
complete            # これまでに登録した全補完を一覧表示
complete -e -c mycmd  # mycmd の補完を消去
```

登録後は、`mycmd `（または `mycmd -`）の後で Tab を押すと、対応するフラグや候補語が Jarvish の他の補完ソースと並んで表示されます。前方一致（フラグ・`-a` 引数語のいずれも）は**大文字小文字を区別します** — `mycmd B` と入力しても `build` として登録した候補には一致しません。

**動的候補（`-a "$(...)"`）**: `-a` の値が（前後の空白を除いて）ちょうど `$(コマンド)` の形をしている場合、Jarvish はそれを静的な単語リストではなく*動的*ソースとして扱います。`コマンド` は Tab を押すたびに `/bin/sh -c` 経由で実行され、その標準出力が候補になります。出力の各行は `値<TAB>説明文` としてパースされます（タブと説明文は省略可 — `値` だけの行でもよく、その場合は spec の `-d` にフォールバックします）。空行はスキップされ、末尾の `\r` は取り除かれます。実行時間は `[completion] external_timeout_ms`（下限 200ms）で打ち切られ、タイムアウト・非ゼロ終了・spawn 失敗はいずれもエラーではなく「この spec からは 0 候補」として扱われます — 同じコマンドの他の spec は引き続き有効で、全体として一致がなければ他の補完ソースにフォールスルーします。1 個の `-a` 文字列の中で静的な単語と `$(...)` を混在させることは**サポートしていません** — `-a` は「静的な単語リスト」か「単一の `$(...)`」のどちらか一方です。

**条件式（`-n`）**: 評価されるのは次の 2 形式のみで、いずれもサブプロセスを起動せずに判定します。
- `__fish_use_subcommand` — コマンド名の後ろにまだフラグ以外の単語（サブコマンド相当）が現れていない間は true（`mycmd -v <Tab>` はフラグのみなので依然として「サブコマンド未出現」扱いです）。
- `__fish_seen_subcommand_from w1 w2 ...` — 挙げた単語のいずれかがコマンド名より後ろに一度でも出現していれば true。

上記以外の `-n` を持つ spec は**補完候補には反映されません**（一切候補を出しません）が、登録自体は保持され `complete` の一覧には表示されます — これはこのフェーズの既知の制限であり、バグではありません。

具体例 — サブコマンドが2種類あり、うち一方が動的に列挙される引数を取る `mycmd`:

```sh
complete -c mycmd -n '__fish_use_subcommand' -a 'start stop'
complete -c mycmd -n '__fish_seen_subcommand_from start' -a "$(mycmd --list-targets)"
```

`mycmd ` の直後で Tab を押すと `start`/`stop` が候補になり、`mycmd start ` の後では `mycmd --list-targets` が実行され、その出力が候補として提示されます。

**再起動をまたいで永続化するには**: プロンプトで直接登録した `complete` の spec はメモリ上にのみ存在し、Jarvish を終了すると失われます。これ（と他の初期設定）を再起動をまたいで永続化するには、同じコマンドを下記の [`rc.jsh`](#-起動スクリプトrcjsh) に書いてください。

### 🏁 起動スクリプト（`rc.jsh`）

`~/.config/jarvish/rc.jsh` は、Jarvish が**対話的に**起動するたびに一度だけ実行されるプレーンテキストの起動スクリプトです — `config.toml` の `[startup].commands` セクションより前、かつ最初のプロンプトが表示される前に実行されます。これはまさに上記の「セッション限り」問題を解決するために存在します: `alias`/`export`/`complete` の呼び出し（や他の任意のビルトイン）を `rc.jsh` に書いておけば、シェルエイリアスやコピペを使わずに再起動のたびに自動で反映されます。

- **配置場所**: `~/.config/jarvish/rc.jsh`（`config.toml` の配置場所の慣習を踏襲）。このファイルが存在しない場合、初回の対話起動時にコメントのみのテンプレートが自動生成されます — 以降は一切上書きされないため、編集内容は安全です。（下記の明示的な `--rcfile` パスは自動生成されません。）
- **CLI オプション**:
  - `--rcfile <PATH>` — デフォルトの `~/.config/jarvish/rc.jsh` の代わりに `<PATH>` を読み込みます。存在しなくても自動生成はされません — 指定パスが見つからない場合は `jarvish: rcfile not found: <PATH>` を stderr に出し、rc スクリプトなしで起動を継続します。デフォルトパスと異なり、明示的な `--rcfile` は `-c` モードでも読み込まれます — `-c` のコマンドを実行する前にロード（実行や `exit` も可能）されます。単体の `-c`（`--rcfile` なし）は rc.jsh に一切触れません。
  - `--no-rc` — rc スクリプトの読み込みを完全にスキップします（デフォルトパスのテンプレート自動生成も含みます）。
  - `--rcfile` と `--no-rc` は同時指定できません（競合エラーになります）。
- **フォーマット**: 1行につき1コマンド。空行はスキップされます。行の先頭の非空白文字が `#` である行は行全体がコメントとしてスキップされます — 行の途中（クォート文字列の中など）に現れる `#` はコメントの開始とは**みなされません**。行継続構文は存在しないため、各コマンドは1行に収めてください。
- **分類器バイパスの保証**: すべての行は、プロンプトで直接入力した場合と同じビルトインディスパッチ経路で実行されます（先頭のエイリアス展開の後、`alias`、`export`、`complete`、`cd`、`source`、通常のコマンドはすべて対話時と全く同じように動作します）— しかし AI の自然言語分類器は**一切経由しません**。AI アシスタントへの質問や依頼のように見える行であっても特別扱いはされず、単なるコマンドとして実行され、コマンドとして存在しなければ「not found」で失敗します。`rc.jsh` は決定的なセットアップのためのものであり、会話のためのものではありません。すべての行でエイリアス展開が行われるため、スクリプトの早い行で定義した `alias` は同じスクリプトの後の行（や、そのスクリプトが `source` する別のスクリプト、その逆方向）からも使えます。
- **実行順序**: `rc.jsh` → `[startup].commands`（`config.toml`）→ 最初のプロンプト。
- **エラー処理**: 失敗した行は、そのコマンド自身のエラーに加えて `jarvish: rc.jsh:<行番号>: command exited with status <code>` というサマリー行を出力し、次の行の実行を継続します。1行の失敗によって `rc.jsh` の実行が途中で中断されることはありません。`exit <code>`（や `restart`）行は意図的なアクションであり失敗したコマンドではないため、`<code>` が非ゼロであってもこのサマリー行は出力されません。
- **スクリプトからの終了**: `exit` 行（または `bye`/`さようなら` のような別れの挨拶）は、対話プロンプトと同様にスクリプトの実行を止めて即座に Jarvish を終了させます。`restart` 行も `-c` を含むすべてのモードで同様に動作します: 「Restarting jarvish...」と出力して単に終了するのではなく、実際に Jarvish は自分自身を再 exec します。
- **history には記録されない**: `rc.jsh` や `source` 経由で実行された行は、対話時や `-c` の行がすべて記録されるのとは異なり、Black Box（`history` ビルトイン / `history.db`）には一切記録されません。これは見落としではなく意図的な仕様です — スクリプトの行は「設定の再生」であり、ユーザーが対話的に打ったコマンドではありません。もし記録すれば、Jarvish を起動するたびに同じ `alias`/`export`/`complete` の呼び出しで `history` がスパムされてしまいます（bash の `source`/`.` が history に追加されないのと同じ方針です）。

例:

```sh
# ~/.config/jarvish/rc.jsh

# エイリアスを永続化する
alias gs="git status"
alias ll="eza --icons -la"

# 社内ツール向けの補完を登録する（上記「カスタム補完」参照）
complete -c mycmd -s v -l verbose -d 'Verbose output'
complete -c mycmd -a 'start stop restart' -d 'Subcommand'
```

#### `source`: 設定の再読み込み、またはスクリプトの実行

`source <path>` はファイルの拡張子で挙動が分岐します:

- **`.toml`**（大文字小文字を区別しない） — `config.toml` を再読み込みし、`[ai]`/`[alias]`/`[export]`/`[prompt]`/`[completion]` をその場で反映します。従来と完全に同一の挙動です: `source ~/.config/jarvish/config.toml` は今までどおり `Loaded ...` サマリーを出力します（上記「設定ファイル」の Tip 参照）。
- **それ以外の拡張子、または拡張子なし** — ファイルを rc スクリプトとして実行します。実行器は `rc.jsh` 本体と全く同じものを使います: 分類器バイパス、`#` コメント/空行の扱い、行番号付きの `jarvish: <file>:<lineno>: ...` エラー、continue-on-error、`exit`/goodbye の伝播、すべて同一の意味論です。大きな `rc.jsh` を複数ファイルに分割して `source` したり、プロンプトからその場限りのスクリプト（`source ./setup.jsh`）を読み込んだりできます。
- **ネスト**: source したスクリプトの中からさらに `source` することもできますが、最大深さは 8 です。これを超える（自分自身を source するケースを含む）と、ハングせずに `jarvish: <file>: source nesting too deep` で停止します。このネスト深さガードは `source` によるネストの**深さ**のみを制限するものであり、1つの深さレベルでスクリプト（やその `source` ツリー全体）が行える作業の**総量**は制限しません。同じ深さレベルで大量の `source` 呼び出しを行うスクリプトや、1行で何千ものコマンドを実行するスクリプトはこのガードでは止まりません — 幅広くファンアウトする、あるいは高コストなスクリプトを節度あるものに保つのはユーザー自身の責任です。
- **終了コード**: スクリプトとしての `source` は、全行成功なら 0、いずれかの行が失敗していれば 1 を返します。スクリプト自体に `exit`/goodbye 行が含まれていた場合はシェルそのものを終了します。
- **ファイルが存在しない場合**: 実際のメッセージ文言は分岐（通るコードパス）によって異なりますが（`.toml` 側は `source ~/.config/jarvish/config.toml` が従来から使っている設定ローダーにそのまま委譲するため、OS の I/O エラーがそのまま出ます）、どちらも終了コード 1 を返す点は共通です: 存在しない**スクリプト**パス（`.toml` 以外の拡張子、または拡張子なし）は `jarvish: source: no such file: <path>` を報告します。存在しない **`.toml`** パスは `jarvish: source: failed to read <path>: <os error>`（例: `No such file or directory (os error 2)`）を報告します。
- **`*.toml` という名前のディレクトリ**: 拡張子だけ見ると設定ファイルのように見えても、実体がディレクトリであるパスを source すると `jarvish: source: <path> is a directory` を報告し終了コード 1 を返します — 設定再読み込みの処理には一切到達しません。

## 🏗️ アーキテクチャ

Jarvish は、高度にモジュール化された4つのコアコンポーネントで構成されています。

```mermaid
graph TB
    User(["ユーザー"]) --> A["Line Editor (reedline)"]
    A --> B["Execution Engine"]
    B --> B1["ビルトインコマンド (cd, exit, alias...)"]
    B --> B2["外部コマンド (PTY + I/O キャプチャ)"]
    B --> D["AI Brain (OpenAI API / Tools)"]
    B2 --> C["Black Box"]
    D --> C
    C --> C1[("history.db (SQLite)")]
    C --> C2[("blobs/ (SHA-256 + zstd)")]
```

| コンポーネント       | 役割                                                                                   |
| :------------------- | :------------------------------------------------------------------------------------- |
| **Line Editor**      | `reedline` ベースのREPL。非同期Gitプロンプト、ハイライト、履歴サジェストを提供。       |
| **Execution Engine** | コマンドのパース、ディスパッチ、そしてPTYセッションを用いた確実なI/Oキャプチャを実行。 |
| **Black Box**        | ターミナルの全記憶を司るストレージエンジン。SQLiteと圧縮Blobのハイブリッド構造。       |
| **AI Brain**         | 自然言語/コマンドの意図分類と、コンテキストを理解した自律的エージェントループを駆動。  |

## 👩‍💻 開発への参加

### Git Hooks のセットアップ

安全な開発のために、pre-push フックを提供しています。

```bash
make install-hooks   # フックをインストール
make uninstall-hooks # フックを削除
```

### コードの検証（CIローカル実行）

```bash
make check  # format, clippy, check, test を一括実行して安全性を確認
```

### CI パイプライン (GitHub Actions)

すべての Push と `main` への Pull Request で以下のCIが実行されます：

- `cargo check --all-targets`
- `cargo test --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
