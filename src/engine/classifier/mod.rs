//! 入力分類器 — コマンド vs 自然言語をアルゴリズムで判定
//!
//! AI API を呼ばずに、ヒューリスティックとリアルタイム PATH 解決で
//! ユーザー入力がシェルコマンドか自然言語かを瞬時に判定する。
//!
//! `which` クレートを用いて `$PATH` を走査し、短寿命 TTL キャッシュで
//! 同一トークンの重複走査を排除する。`brew install` 等で新しいバイナリが
//! 追加された場合でも TTL 経過後に自動で反映される。

mod goodbye;
mod patterns;

pub use goodbye::is_ai_goodbye_response;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use tracing::{debug, info};

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

/// PATH lookup キャッシュの TTL（秒）。
const PATH_CACHE_TTL_SECS: u64 = 5;

/// アルゴリズムベースの入力分類器（TTL キャッシュ付き PATH 解決）
///
/// `which::which()` を用いて `$PATH` 上の実行可能ファイルを検索する。
/// キーストロークごとのハイライト呼び出しによる CPU 負荷を抑えるため、
/// コマンド名 → 存在有無のマッピングを短寿命キャッシュで保持する。
pub struct InputClassifier {
    /// PATH lookup キャッシュ: コマンド名 → (存在するか, キャッシュ時刻)
    path_cache: Mutex<HashMap<String, (bool, Instant)>>,
}

impl InputClassifier {
    pub fn new() -> Self {
        info!(
            "InputClassifier initialized (TTL-cached PATH resolution, TTL={PATH_CACHE_TTL_SECS}s)"
        );
        Self {
            path_cache: Mutex::new(HashMap::new()),
        }
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

        if Self::is_goodbye_pattern(trimmed) {
            debug!(input = %trimmed, reason = "goodbye_pattern", "Classified as Goodbye");
            return InputType::Goodbye;
        }

        if self.is_jarvis_trigger(trimmed) {
            debug!(input = %trimmed, reason = "jarvis_trigger", "Classified as NaturalLanguage");
            return InputType::NaturalLanguage;
        }

        if self.is_natural_language_pattern(trimmed) {
            debug!(input = %trimmed, reason = "nl_pattern", "Classified as NaturalLanguage");
            return InputType::NaturalLanguage;
        }

        let first_token = Self::first_token(trimmed);

        if Self::is_path_execution(first_token) {
            debug!(input = %trimmed, first_token = %first_token, reason = "path_execution", "Classified as Command");
            return InputType::Command;
        }

        if self.is_command_in_path(first_token) {
            debug!(input = %trimmed, first_token = %first_token, reason = "path_lookup", "Classified as Command");
            return InputType::Command;
        }

        if Self::has_shell_syntax(trimmed) {
            debug!(input = %trimmed, reason = "shell_syntax", "Classified as Command");
            return InputType::Command;
        }

        debug!(input = %trimmed, reason = "default", "Classified as NaturalLanguage");
        InputType::NaturalLanguage
    }

    /// 入力文字列から先頭トークン（空白前の最初の語）を取得する。
    fn first_token(input: &str) -> &str {
        input.split_whitespace().next().unwrap_or("")
    }

    /// 先頭トークンが `$PATH` 上の実行可能ファイルとして存在するか。
    ///
    /// TTL キャッシュにより、同一トークンに対する `which::which()` の
    /// 重複呼び出しを排除する。TTL 経過後は自動で再走査される。
    fn is_command_in_path(&self, token: &str) -> bool {
        let now = Instant::now();

        if let Ok(cache) = self.path_cache.lock() {
            if let Some(&(result, cached_at)) = cache.get(token) {
                if now.duration_since(cached_at).as_secs() < PATH_CACHE_TTL_SECS {
                    return result;
                }
            }
        }

        let result = which::which(token).is_ok();

        if let Ok(mut cache) = self.path_cache.lock() {
            cache.insert(token.to_string(), (result, now));
        }

        result
    }

    /// PATH lookup キャッシュをクリアする。
    #[cfg(test)]
    fn clear_path_cache(&self) {
        if let Ok(mut cache) = self.path_cache.lock() {
            cache.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn test_classifier() -> InputClassifier {
        InputClassifier::new()
    }

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
        assert_eq!(c.classify("jarvis"), InputType::NaturalLanguage);
    }

    #[test]
    fn jarvis_trigger_ignores_words_starting_with_jarvis() {
        let c = test_classifier();
        assert!(
            !c.is_jarvis_trigger("jarvish"),
            "jarvish must not match Jarvis trigger"
        );
        assert!(
            !c.is_jarvis_trigger("jarvisbot --help"),
            "jarvisbot must not match Jarvis trigger"
        );
        assert!(c.is_jarvis_trigger("jarvis"));
        assert!(c.is_jarvis_trigger("jarvis help"));
        assert!(c.is_jarvis_trigger("jarvis, help"));
        assert!(c.is_jarvis_trigger("hey jarvis, help"));
        assert!(
            !c.is_jarvis_trigger("hey jarvish"),
            "hey jarvish must not match Jarvis trigger"
        );
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

    #[test]
    fn realtime_path_resolution_finds_common_commands() {
        let c = test_classifier();
        assert_eq!(c.classify("ls"), InputType::Command);
        assert_eq!(c.classify("cat file.txt"), InputType::Command);
    }

    #[test]
    #[serial]
    fn realtime_path_resolution_reflects_new_path() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let c = test_classifier();
        let fake_cmd = "zzz_jarvish_test_fake_cmd_42";

        assert_eq!(c.classify(fake_cmd), InputType::NaturalLanguage);

        let tmp_dir = std::env::temp_dir().join("jarvish_test_realtime");
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&tmp_dir).unwrap();
        let fake_bin = tmp_dir.join(fake_cmd);
        fs::write(&fake_bin, "#!/bin/sh\necho hello\n").unwrap();
        fs::set_permissions(&fake_bin, fs::Permissions::from_mode(0o755)).unwrap();

        let original_path = std::env::var("PATH").unwrap();
        let new_path = format!("{}:{original_path}", tmp_dir.display());
        unsafe {
            std::env::set_var("PATH", &new_path);
        }

        c.clear_path_cache();

        assert_eq!(
            c.classify(fake_cmd),
            InputType::Command,
            "should be Command after PATH change and cache clear"
        );

        unsafe {
            std::env::set_var("PATH", &original_path);
        }
        let _ = fs::remove_dir_all(&tmp_dir);

        c.clear_path_cache();

        assert_eq!(c.classify(fake_cmd), InputType::NaturalLanguage);
    }

    #[test]
    fn classify_apostrophe_input() {
        let c = test_classifier();
        assert_eq!(c.classify("I'm tired, Jarvis"), InputType::NaturalLanguage);
    }

    #[test]
    fn classify_semicolon_command() {
        let c = test_classifier();
        assert_eq!(c.classify("echo hello; echo world"), InputType::Command);
    }

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
        assert_eq!(c.classify("bye jarvis"), InputType::Goodbye);
        assert_eq!(c.classify("goodbye sir"), InputType::Goodbye);
        assert_eq!(c.classify("see you later"), InputType::Goodbye);
    }

    #[test]
    fn classify_goodbye_false_positives() {
        let c = test_classifier();
        assert_ne!(
            c.classify("say goodbye to the old config file and update"),
            InputType::Goodbye
        );
        assert_ne!(
            c.classify("echo goodbye world from here today"),
            InputType::Goodbye
        );
    }
}
