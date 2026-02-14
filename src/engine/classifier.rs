//! 入力分類器 — コマンド vs 自然言語をアルゴリズムで判定
//!
//! AI API を呼ばずに、ヒューリスティックと PATH 解決で
//! ユーザー入力がシェルコマンドか自然言語かを瞬時に判定する。

use std::collections::HashSet;
use std::env;
use std::fs;

use tracing::{debug, info, warn};

/// 入力の分類結果
#[derive(Debug, Clone, PartialEq)]
pub enum InputType {
    /// シェルコマンド（直接実行、AI 不要）
    Command,
    /// 自然言語（AI に送信して応答を生成）
    NaturalLanguage,
}

/// アルゴリズムベースの入力分類器
///
/// 起動時に PATH 内の実行可能コマンド名を `HashSet` にキャッシュし、
/// O(1) でコマンド判定を行う。
pub struct InputClassifier {
    /// PATH 内の実行可能コマンド名のキャッシュ
    path_commands: HashSet<String>,
}

impl InputClassifier {
    /// PATH 環境変数を走査し、実行可能コマンド名をキャッシュして初期化する。
    pub fn new() -> Self {
        let path_commands = Self::build_path_cache();
        info!(
            cached_commands = path_commands.len(),
            "InputClassifier initialized with PATH cache"
        );
        Self { path_commands }
    }

    /// ユーザー入力を分類する。
    ///
    /// 判定ロジック（優先順位順）:
    /// 1. Jarvis トリガー → NaturalLanguage
    /// 2. 自然言語パターン → NaturalLanguage
    /// 3. パス実行パターン → Command
    /// 4. PATH 内コマンド → Command
    /// 5. シェル構文シグナル → Command
    /// 6. デフォルト → NaturalLanguage
    pub fn classify(&self, input: &str) -> InputType {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return InputType::Command;
        }

        // 1. Jarvis トリガー
        if self.is_jarvis_trigger(trimmed) {
            debug!(input = %trimmed, reason = "jarvis_trigger", "Classified as NaturalLanguage");
            return InputType::NaturalLanguage;
        }

        // 2. 自然言語パターン
        if self.is_natural_language_pattern(trimmed) {
            debug!(input = %trimmed, reason = "nl_pattern", "Classified as NaturalLanguage");
            return InputType::NaturalLanguage;
        }

        // 先頭トークンを抽出
        let first_token = Self::first_token(trimmed);

        // 3. パス実行パターン（./script.sh, /usr/bin/foo, ~/bin/tool）
        if Self::is_path_execution(first_token) {
            debug!(input = %trimmed, first_token = %first_token, reason = "path_execution", "Classified as Command");
            return InputType::Command;
        }

        // 4. PATH 内コマンド（最も強力なヒューリスティック）
        if self.is_command_in_path(first_token) {
            debug!(input = %trimmed, first_token = %first_token, reason = "path_lookup", "Classified as Command");
            return InputType::Command;
        }

        // 5. シェル構文シグナル
        if Self::has_shell_syntax(trimmed) {
            debug!(input = %trimmed, reason = "shell_syntax", "Classified as Command");
            return InputType::Command;
        }

        // 6. デフォルト: 自然言語として AI に委ねる
        debug!(input = %trimmed, reason = "default", "Classified as NaturalLanguage");
        InputType::NaturalLanguage
    }

    // ========== プライベートヘルパー ==========

    /// PATH 環境変数を走査し、実行可能ファイル名を HashSet に格納する。
    fn build_path_cache() -> HashSet<String> {
        let mut commands = HashSet::new();

        let path_var = match env::var("PATH") {
            Ok(p) => p,
            Err(_) => {
                warn!("PATH environment variable not set, classifier will rely on heuristics only");
                return commands;
            }
        };

        for dir in env::split_paths(&path_var) {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue, // 読めないディレクトリはスキップ
            };

            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    // 実行可能かの簡易チェック（Unix: ファイルであること）
                    // NOTE: fs::metadata はシンボリックリンクを辿る（entry.metadata は辿らない）
                    if let Ok(metadata) = fs::metadata(entry.path()) {
                        if metadata.is_file() {
                            commands.insert(name.to_string());
                        }
                    }
                }
            }
        }

        commands
    }

    /// Jarvis に話しかけるトリガーパターンかを判定する。
    fn is_jarvis_trigger(&self, input: &str) -> bool {
        let lower = input.to_lowercase();
        // "jarvis" / "jarvis," / "hey jarvis" / "j," で始まる
        lower.starts_with("jarvis")
            || lower.starts_with("hey jarvis")
            || lower.starts_with("j,")
            || lower.starts_with("j ") && !self.is_command_in_path("j")
    }

    /// 自然言語パターン（疑問詞、依頼表現 等）にマッチするかを判定する。
    fn is_natural_language_pattern(&self, input: &str) -> bool {
        let lower = input.to_lowercase();

        // 末尾が ? で終わる
        if lower.ends_with('?') {
            return true;
        }

        // 先頭の単語を取得
        let first_word = lower.split_whitespace().next().unwrap_or("");

        // 英語の疑問詞・助動詞で始まる（2語以上の場合のみ）
        let has_multiple_words = lower.contains(' ');
        if has_multiple_words {
            let question_starters = [
                "what", "how", "why", "where", "when", "who", "which", "can", "could", "would",
                "should", "shall", "is", "are", "was", "were", "am", "do", "does", "did", "tell",
                "explain", "describe", "show", "please", "help",
            ];

            if question_starters.contains(&first_word) {
                // ただし PATH 上にも同名コマンドが存在する場合はコマンド優先
                // 例: "which python" は自然言語ではなくコマンド
                if !self.is_command_in_path(first_word) {
                    return true;
                }
            }
        }

        // 日本語パターン
        if lower.ends_with("して")
            || lower.ends_with("してください")
            || lower.ends_with("とは")
            || lower.ends_with("教えて")
            || lower.ends_with("ですか")
            || lower.ends_with("ますか")
            || lower.ends_with("なに")
            || lower.ends_with("何")
        {
            return true;
        }

        false
    }

    /// 先頭トークンがパス実行パターン（./foo, ../foo, /usr/bin/foo, ~/foo）か。
    fn is_path_execution(first_token: &str) -> bool {
        first_token.starts_with("./")
            || first_token.starts_with("../")
            || first_token.starts_with('/')
            || first_token.starts_with("~/")
    }

    /// 先頭トークンが PATH キャッシュに存在するか。
    fn is_command_in_path(&self, token: &str) -> bool {
        self.path_commands.contains(token)
    }

    /// 入力にシェル構文（パイプ、論理演算子、セミコロン、変数展開、代入）が含まれるか。
    fn has_shell_syntax(input: &str) -> bool {
        // パイプ、論理演算子、セミコロン
        input.contains('|')
            || input.contains(" && ")
            || input.contains(" || ")
            || input.contains(';')
            // 変数展開（先頭が $ で始まる）
            || input.starts_with('$')
            // 環境変数代入パターン（KEY=value）
            || input.split_whitespace().any(|token| {
                token.contains('=')
                    && token.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            })
    }

    /// 入力文字列から先頭トークン（空白前の最初の語）を取得する。
    fn first_token(input: &str) -> &str {
        input.split_whitespace().next().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の分類器を作成する。
    /// 実際の PATH を使うため、環境依存だが主要コマンド（ls, git 等）は存在するはず。
    fn test_classifier() -> InputClassifier {
        InputClassifier::new()
    }

    // ── InputType: コマンド判定 ──

    #[test]
    fn classify_simple_command() {
        let c = test_classifier();
        assert_eq!(c.classify("ls"), InputType::Command);
        assert_eq!(c.classify("ls -la"), InputType::Command);
    }

    #[test]
    fn classify_git_commands() {
        let c = test_classifier();
        assert_eq!(c.classify("git status"), InputType::Command);
        assert_eq!(c.classify("git log --oneline"), InputType::Command);
    }

    #[test]
    fn classify_common_commands() {
        let c = test_classifier();
        assert_eq!(c.classify("echo hello"), InputType::Command);
        assert_eq!(c.classify("cat file.txt"), InputType::Command);
        assert_eq!(c.classify("grep error log.txt"), InputType::Command);
        assert_eq!(c.classify("mkdir new_dir"), InputType::Command);
    }

    #[test]
    fn classify_path_execution() {
        let c = test_classifier();
        assert_eq!(c.classify("./script.sh"), InputType::Command);
        assert_eq!(c.classify("../bin/tool"), InputType::Command);
        assert_eq!(c.classify("/usr/bin/python3"), InputType::Command);
        assert_eq!(c.classify("~/bin/my_tool"), InputType::Command);
    }

    #[test]
    fn classify_pipe_and_operators() {
        let c = test_classifier();
        assert_eq!(c.classify("cat file.txt | grep error"), InputType::Command);
        assert_eq!(c.classify("make && make test"), InputType::Command);
        assert_eq!(c.classify("cmd1 || cmd2"), InputType::Command);
    }

    #[test]
    fn classify_variable_expansion() {
        let c = test_classifier();
        assert_eq!(c.classify("$HOME/bin/tool"), InputType::Command);
    }

    // ── InputType: 自然言語判定 ──

    #[test]
    fn classify_jarvis_trigger() {
        let c = test_classifier();
        assert_eq!(c.classify("jarvis, help me"), InputType::NaturalLanguage);
        assert_eq!(
            c.classify("Jarvis what is this?"),
            InputType::NaturalLanguage
        );
        assert_eq!(c.classify("hey jarvis"), InputType::NaturalLanguage);
        assert_eq!(c.classify("j, commit please"), InputType::NaturalLanguage);
    }

    #[test]
    fn classify_question_patterns() {
        let c = test_classifier();
        assert_eq!(
            c.classify("what does this error mean?"),
            InputType::NaturalLanguage
        );
        assert_eq!(c.classify("how do I fix this?"), InputType::NaturalLanguage);
        assert_eq!(
            c.classify("why did the build fail?"),
            InputType::NaturalLanguage
        );
        assert_eq!(
            c.classify("where is the config file?"),
            InputType::NaturalLanguage
        );
    }

    #[test]
    fn classify_question_mark_ending() {
        let c = test_classifier();
        assert_eq!(c.classify("what's the error?"), InputType::NaturalLanguage);
        assert_eq!(c.classify("さっきのエラーは?"), InputType::NaturalLanguage);
    }

    #[test]
    fn classify_request_patterns() {
        let c = test_classifier();
        assert_eq!(
            c.classify("please explain the output"),
            InputType::NaturalLanguage
        );
        assert_eq!(c.classify("help me debug this"), InputType::NaturalLanguage);
        assert_eq!(c.classify("explain this error"), InputType::NaturalLanguage);
        assert_eq!(
            c.classify("tell me about git rebase"),
            InputType::NaturalLanguage
        );
    }

    #[test]
    fn classify_japanese_patterns() {
        let c = test_classifier();
        assert_eq!(c.classify("エラーを教えて"), InputType::NaturalLanguage);
        assert_eq!(
            c.classify("このファイルを修正して"),
            InputType::NaturalLanguage
        );
        assert_eq!(c.classify("gitとは"), InputType::NaturalLanguage);
        assert_eq!(c.classify("これはなんですか"), InputType::NaturalLanguage);
    }

    #[test]
    fn classify_empty_input() {
        let c = test_classifier();
        assert_eq!(c.classify(""), InputType::Command);
        assert_eq!(c.classify("   "), InputType::Command);
    }

    // ── PATH キャッシュ ──

    #[test]
    fn path_cache_contains_common_commands() {
        let c = test_classifier();
        // ls と cat は macOS/Linux のどちらにも存在するはず
        assert!(
            c.path_commands.contains("ls"),
            "PATH cache should contain 'ls'"
        );
        assert!(
            c.path_commands.contains("cat"),
            "PATH cache should contain 'cat'"
        );
    }

    #[test]
    fn path_cache_does_not_contain_nonsense() {
        let c = test_classifier();
        assert!(!c.path_commands.contains("xyzzy_nonexistent_command_12345"));
    }

    // ── エッジケース ──

    #[test]
    fn classify_apostrophe_input() {
        let c = test_classifier();
        // "I'm tired, Jarvis" のようなアポストロフィ入力は自然言語
        assert_eq!(c.classify("I'm tired, Jarvis"), InputType::NaturalLanguage);
    }

    #[test]
    fn classify_semicolon_command() {
        let c = test_classifier();
        assert_eq!(c.classify("echo hello; echo world"), InputType::Command);
    }
}
