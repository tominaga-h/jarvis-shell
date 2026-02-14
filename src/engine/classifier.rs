//! 入力分類器 — コマンド vs 自然言語をアルゴリズムで判定
//!
//! AI API を呼ばずに、ヒューリスティックと PATH 解決で
//! ユーザー入力がシェルコマンドか自然言語かを瞬時に判定する。

use std::collections::HashSet;
use std::env;
use std::fs;
use std::sync::RwLock;

use tracing::{debug, info, warn};

/// 入力の分類結果
#[derive(Debug, Clone, PartialEq)]
pub enum InputType {
    /// シェルコマンド（直接実行、AI 不要）
    Command,
    /// 自然言語（AI に送信して応答を生成）
    NaturalLanguage,
    /// Goodbye（シェルを終了する）
    Goodbye,
}

/// アルゴリズムベースの入力分類器
///
/// 起動時に PATH 内の実行可能コマンド名を `HashSet` にキャッシュし、
/// O(1) でコマンド判定を行う。
/// `RwLock` による内部可変性を持ち、`export PATH=...` 等で PATH が変更された際に
/// キャッシュを動的にリロードできる。
pub struct InputClassifier {
    /// PATH 内の実行可能コマンド名のキャッシュ（RwLock で動的リロード可能）
    path_commands: RwLock<HashSet<String>>,
}

impl InputClassifier {
    /// PATH 環境変数を走査し、実行可能コマンド名をキャッシュして初期化する。
    pub fn new() -> Self {
        let path_commands = Self::build_path_cache();
        info!(
            cached_commands = path_commands.len(),
            "InputClassifier initialized with PATH cache"
        );
        Self {
            path_commands: RwLock::new(path_commands),
        }
    }

    /// PATH キャッシュを再構築する。
    ///
    /// `export PATH=...` や `unset PATH` で PATH 環境変数が変更された際に呼び出す。
    /// 現在の PATH 環境変数を再スキャンし、キャッシュを差し替える。
    pub fn reload_path_cache(&self) {
        let new_cache = Self::build_path_cache();
        info!(cached_commands = new_cache.len(), "PATH cache reloaded");
        *self.path_commands.write().unwrap() = new_cache;
    }

    /// PATH キャッシュの読み取りロックガードを返す。
    ///
    /// `JarvishCompleter` 等、外部から PATH コマンド一覧を参照する際に使用する。
    pub fn path_commands(&self) -> std::sync::RwLockReadGuard<'_, HashSet<String>> {
        self.path_commands.read().unwrap()
    }

    /// ユーザー入力を分類する。
    ///
    /// 判定ロジック（優先順位順）:
    /// 0. Goodbye パターン → Goodbye（最優先）
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

        // 0. Goodbye パターン（最優先）
        if Self::is_goodbye_pattern(trimmed) {
            debug!(input = %trimmed, reason = "goodbye_pattern", "Classified as Goodbye");
            return InputType::Goodbye;
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

    /// ユーザー入力が Goodbye パターンにマッチするかを判定する。
    ///
    /// 英語・日本語の別れの挨拶を検出する。
    /// 誤検出を防ぐため、入力が短い（概ね3語以下）場合に限定する。
    fn is_goodbye_pattern(input: &str) -> bool {
        let lower = input.to_lowercase();

        // Jarvis 呼びかけプレフィックスを除去して本文を取得
        let body = Self::strip_jarvis_prefix(&lower);

        // 本文が長すぎる場合は goodbye 意図ではないと判定（"say goodbye to X" 等を除外）
        let word_count = body.split_whitespace().count();
        if word_count > 4 {
            return false;
        }

        // 英語 goodbye パターン（完全一致 or 先頭一致）
        let goodbye_phrases = [
            "bye",
            "bye bye",
            "bye-bye",
            "byebye",
            "goodbye",
            "good bye",
            "good-bye",
            "see you",
            "see ya",
            "good night",
            "goodnight",
            "farewell",
            "ciao",
        ];

        for phrase in &goodbye_phrases {
            if body == *phrase || body.starts_with(&format!("{phrase} ")) {
                return true;
            }
        }

        // 日本語 goodbye パターン（末尾一致 or 完全一致）
        let jp_patterns = [
            "さようなら",
            "さよなら",
            "おやすみ",
            "おやすみなさい",
            "バイバイ",
            "ばいばい",
            "じゃあね",
            "じゃね",
            "またね",
            "また明日",
            "またあとで",
            "おつかれ",
            "おつかれさま",
            "おつかれさまでした",
            "お疲れ様",
            "お疲れさま",
            "お疲れさまでした",
        ];

        for pattern in &jp_patterns {
            if body == *pattern || body.ends_with(pattern) {
                return true;
            }
        }

        false
    }

    /// Jarvis 呼びかけプレフィックス（"jarvis, ", "hey jarvis, ", "j, " 等）を除去する。
    fn strip_jarvis_prefix(input: &str) -> &str {
        let prefixes = [
            "hey jarvis, ",
            "hey jarvis,",
            "hey jarvis ",
            "jarvis, ",
            "jarvis,",
            "jarvis ",
            "j, ",
            "j,",
        ];

        for prefix in &prefixes {
            if let Some(rest) = input.strip_prefix(prefix) {
                return rest.trim();
            }
        }

        input
    }

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
        self.path_commands.read().unwrap().contains(token)
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

/// AI の応答テキストが Goodbye（別れの挨拶）を含むかを判定する。
///
/// 誤検出を防ぐため、応答テキストの末尾付近（最後の3行）のみを検査する。
/// AI が文中で "goodbye" に言及しただけの場合はトリガーしない。
pub fn is_ai_goodbye_response(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    // 末尾3行を取得して検査対象とする
    let lines: Vec<&str> = trimmed.lines().collect();
    let tail_start = if lines.len() > 3 { lines.len() - 3 } else { 0 };
    let tail = lines[tail_start..].join("\n").to_lowercase();

    let farewell_patterns = [
        // 英語
        "goodbye",
        "good bye",
        "farewell",
        "signing off",
        "until next time",
        "see you later",
        "see you soon",
        "good night",
        "take care",
        // 日本語
        "さようなら",
        "さよなら",
        "おやすみなさい",
        "良い夜を",
        "良い一日を",
        "お疲れ様",
        "お疲れさま",
        "またお会い",
    ];

    farewell_patterns.iter().any(|p| tail.contains(p))
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
        let cache = c.path_commands();
        // ls と cat は macOS/Linux のどちらにも存在するはず
        assert!(cache.contains("ls"), "PATH cache should contain 'ls'");
        assert!(cache.contains("cat"), "PATH cache should contain 'cat'");
    }

    #[test]
    fn path_cache_does_not_contain_nonsense() {
        let c = test_classifier();
        assert!(!c
            .path_commands()
            .contains("xyzzy_nonexistent_command_12345"));
    }

    #[test]
    fn reload_path_cache_reflects_new_path() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let c = test_classifier();

        // テスト用の架空コマンド名（"jarvis" で始まると Jarvis トリガーに引っかかるので注意）
        let fake_cmd = "zzz_jarvish_test_fake_cmd_42";
        assert!(
            !c.path_commands().contains(fake_cmd),
            "fake command should not exist before reload"
        );
        assert_eq!(
            c.classify(fake_cmd),
            InputType::NaturalLanguage,
            "unknown command should be classified as NaturalLanguage"
        );

        // 一時ディレクトリを作成し、架空コマンドを配置
        let tmp_dir = std::env::temp_dir().join("jarvish_test_path_reload");
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&tmp_dir).expect("failed to create temp dir");
        let fake_bin = tmp_dir.join(fake_cmd);
        fs::write(&fake_bin, "#!/bin/sh\necho hello\n").expect("failed to write fake bin");
        fs::set_permissions(&fake_bin, fs::Permissions::from_mode(0o755))
            .expect("failed to set permissions");

        // PATH を一時的に変更
        let original_path = std::env::var("PATH").unwrap();
        let new_path = format!("{}:{}", tmp_dir.display(), original_path);
        // SAFETY: テストはシングルスレッドで実行（cargo test はデフォルトでシリアル実行可能）
        unsafe {
            std::env::set_var("PATH", &new_path);
        }

        // リロード前: キャッシュはまだ古いので NaturalLanguage
        assert_eq!(
            c.classify(fake_cmd),
            InputType::NaturalLanguage,
            "should still be NaturalLanguage before reload"
        );

        // リロード
        c.reload_path_cache();

        // リロード後: 新しいコマンドが Command として認識される
        assert!(
            c.path_commands().contains(fake_cmd),
            "fake command should be in cache after reload"
        );
        assert_eq!(
            c.classify(fake_cmd),
            InputType::Command,
            "should be classified as Command after reload"
        );

        // クリーンアップ: PATH を元に戻す
        unsafe {
            std::env::set_var("PATH", &original_path);
        }
        let _ = fs::remove_dir_all(&tmp_dir);
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

    // ── InputType: Goodbye 判定 ──

    #[test]
    fn classify_goodbye_english() {
        let c = test_classifier();
        assert_eq!(c.classify("bye"), InputType::Goodbye);
        assert_eq!(c.classify("Bye"), InputType::Goodbye);
        assert_eq!(c.classify("BYE"), InputType::Goodbye);
        assert_eq!(c.classify("bye bye"), InputType::Goodbye);
        assert_eq!(c.classify("bye-bye"), InputType::Goodbye);
        assert_eq!(c.classify("goodbye"), InputType::Goodbye);
        assert_eq!(c.classify("Goodbye"), InputType::Goodbye);
        assert_eq!(c.classify("good bye"), InputType::Goodbye);
        assert_eq!(c.classify("farewell"), InputType::Goodbye);
        assert_eq!(c.classify("see you"), InputType::Goodbye);
        assert_eq!(c.classify("see ya"), InputType::Goodbye);
        assert_eq!(c.classify("good night"), InputType::Goodbye);
        assert_eq!(c.classify("goodnight"), InputType::Goodbye);
        assert_eq!(c.classify("ciao"), InputType::Goodbye);
    }

    #[test]
    fn classify_goodbye_japanese() {
        let c = test_classifier();
        assert_eq!(c.classify("さようなら"), InputType::Goodbye);
        assert_eq!(c.classify("さよなら"), InputType::Goodbye);
        assert_eq!(c.classify("おやすみ"), InputType::Goodbye);
        assert_eq!(c.classify("おやすみなさい"), InputType::Goodbye);
        assert_eq!(c.classify("バイバイ"), InputType::Goodbye);
        assert_eq!(c.classify("ばいばい"), InputType::Goodbye);
        assert_eq!(c.classify("じゃあね"), InputType::Goodbye);
        assert_eq!(c.classify("じゃね"), InputType::Goodbye);
        assert_eq!(c.classify("またね"), InputType::Goodbye);
        assert_eq!(c.classify("また明日"), InputType::Goodbye);
        assert_eq!(c.classify("おつかれ"), InputType::Goodbye);
        assert_eq!(c.classify("おつかれさま"), InputType::Goodbye);
        assert_eq!(c.classify("おつかれさまでした"), InputType::Goodbye);
        assert_eq!(c.classify("お疲れ様"), InputType::Goodbye);
    }

    #[test]
    fn classify_goodbye_with_jarvis_prefix() {
        let c = test_classifier();
        assert_eq!(c.classify("jarvis, goodbye"), InputType::Goodbye);
        assert_eq!(c.classify("Jarvis goodbye"), InputType::Goodbye);
        assert_eq!(c.classify("hey jarvis, bye"), InputType::Goodbye);
        assert_eq!(c.classify("j, bye"), InputType::Goodbye);
        assert_eq!(c.classify("jarvis, おやすみ"), InputType::Goodbye);
    }

    #[test]
    fn classify_goodbye_with_trailing_words() {
        let c = test_classifier();
        // 短い追加語は許容する
        assert_eq!(c.classify("bye jarvis"), InputType::Goodbye);
        assert_eq!(c.classify("goodbye sir"), InputType::Goodbye);
        assert_eq!(c.classify("see you later"), InputType::Goodbye);
    }

    #[test]
    fn classify_goodbye_false_positives() {
        let c = test_classifier();
        // 長い文の中に goodbye が含まれる場合は goodbye として扱わない
        assert_ne!(
            c.classify("say goodbye to the old config file and update"),
            InputType::Goodbye
        );
        assert_ne!(
            c.classify("echo goodbye world from here today"),
            InputType::Goodbye
        );
    }

    // ── AI Goodbye レスポンス検出 ──

    #[test]
    fn ai_goodbye_response_english() {
        assert!(is_ai_goodbye_response("Goodbye, sir. It was a pleasure."));
        assert!(is_ai_goodbye_response(
            "I've completed the task.\nFarewell, sir."
        ));
        assert!(is_ai_goodbye_response(
            "That's all done.\nUntil next time, sir."
        ));
        assert!(is_ai_goodbye_response("Take care, sir. Signing off."));
    }

    #[test]
    fn ai_goodbye_response_japanese() {
        assert!(is_ai_goodbye_response("承知しました。さようなら。"));
        assert!(is_ai_goodbye_response(
            "タスクは完了しました。\nおやすみなさい。"
        ));
        assert!(is_ai_goodbye_response("お疲れ様でした。良い一日を。"));
    }

    #[test]
    fn ai_goodbye_response_not_goodbye() {
        // 空テキスト
        assert!(!is_ai_goodbye_response(""));
        // farewell パターンを含まない通常応答
        assert!(!is_ai_goodbye_response(
            "Here is the command you need: ls -la"
        ));
        assert!(!is_ai_goodbye_response("エラーの原因はこちらです。"));
    }

    #[test]
    fn ai_goodbye_response_only_checks_tail() {
        // 先頭に goodbye があっても末尾3行になければ検出しない
        let long_response = "Goodbye was mentioned here.\n\
                             Line 2\n\
                             Line 3\n\
                             Line 4\n\
                             Line 5\n\
                             This is just a normal response.";
        assert!(!is_ai_goodbye_response(long_response));
    }
}
