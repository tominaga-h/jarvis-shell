//! AST 型定義 — パイプライン構造の構造化表現

/// I/O リダイレクト
#[derive(Debug, Clone, PartialEq)]
pub enum Redirect {
    /// `> file` — stdout を上書き
    StdoutOverwrite(String),
    /// `>> file` — stdout に追記
    StdoutAppend(String),
    /// `< file` — stdin をファイルから読み込み
    StdinFrom(String),
}

/// パイプラインの 1 セグメント（単一コマンド）
#[derive(Debug, Clone, PartialEq)]
pub struct SimpleCommand {
    /// コマンド名（例: "git"）
    pub cmd: String,
    /// コマンド引数（例: ["log", "--oneline"]）
    pub args: Vec<String>,
    /// このコマンドに付与されたリダイレクト
    pub redirects: Vec<Redirect>,
}

/// パイプ（`|`）で接続された一連のコマンド
#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub commands: Vec<SimpleCommand>,
}

impl Pipeline {
    /// パイプラインの最後のコマンドが `ai` であれば、その引数（プロンプト）と、
    /// AI コマンドを除いた新しい Pipeline を返す。
    ///
    /// 以下の場合は `None` を返す:
    /// - 末尾のコマンドが `ai` でない
    /// - `ai` に引数（プロンプト）が指定されていない
    /// - `ai` の手前にコマンドがない（`ai` 単独）
    pub fn extract_ai_filter(&self) -> Option<(String, Pipeline)> {
        let last = self.commands.last()?;
        if last.cmd != "ai" {
            return None;
        }
        let prompt = last.args.join(" ");
        if prompt.is_empty() {
            return None;
        }
        let remaining = Pipeline {
            commands: self.commands[..self.commands.len() - 1].to_vec(),
        };
        if remaining.commands.is_empty() {
            return None;
        }
        Some((prompt, remaining))
    }
}

/// コマンドリストの接続演算子
#[derive(Debug, Clone, PartialEq)]
pub enum Connector {
    /// `&&` — 前のコマンドが成功 (exit_code == 0) した場合のみ次を実行
    And,
    /// `||` — 前のコマンドが失敗 (exit_code != 0) した場合のみ次を実行
    Or,
    /// `;` — 前のコマンドの結果に関わらず次を実行
    Semi,
}

/// `&&`, `||`, `;` で接続された一連のパイプライン
#[derive(Debug, Clone, PartialEq)]
pub struct CommandList {
    /// 先頭のパイプライン
    pub first: Pipeline,
    /// (接続演算子, パイプライン) のペアのリスト
    pub rest: Vec<(Connector, Pipeline)>,
}

/// パースエラー
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
