//! パターン検出 — Goodbye / Jarvis トリガー / 自然言語 / パス実行 / シェル構文

impl super::InputClassifier {
    /// ユーザー入力が Goodbye パターンにマッチするかを判定する。
    ///
    /// 英語・日本語の別れの挨拶を検出する。
    /// 誤検出を防ぐため、入力が短い（概ね3語以下）場合に限定する。
    pub(super) fn is_goodbye_pattern(input: &str) -> bool {
        let lower = input.to_lowercase();
        let body = Self::strip_jarvis_prefix(&lower);

        let word_count = body.split_whitespace().count();
        if word_count > 4 {
            return false;
        }

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
    pub(super) fn strip_jarvis_prefix(input: &str) -> &str {
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

    /// Jarvis に話しかけるトリガーパターンかを判定する。
    ///
    /// "jarvis" / "hey jarvis" の直後が英数字の場合は別コマンド
    /// （例: `jarvish`）とみなしトリガーしない。
    pub(super) fn is_jarvis_trigger(&self, input: &str) -> bool {
        let lower = input.to_lowercase();

        let jarvis_word = lower.starts_with("jarvis")
            && !lower
                .as_bytes()
                .get(6)
                .is_some_and(|b| b.is_ascii_alphanumeric());

        let hey_jarvis_word = lower.starts_with("hey jarvis")
            && !lower
                .as_bytes()
                .get(10)
                .is_some_and(|b| b.is_ascii_alphanumeric());

        jarvis_word
            || hey_jarvis_word
            || lower.starts_with("j,")
            || lower.starts_with("j ") && !self.is_command_in_path("j")
    }

    /// 自然言語パターン（疑問詞、依頼表現 等）にマッチするかを判定する。
    pub(super) fn is_natural_language_pattern(&self, input: &str) -> bool {
        let lower = input.to_lowercase();

        if lower.ends_with('?') {
            return true;
        }

        let first_word = lower.split_whitespace().next().unwrap_or("");

        let has_multiple_words = lower.contains(' ');
        if has_multiple_words {
            let question_starters = [
                "what", "how", "why", "where", "when", "who", "which", "can", "could", "would",
                "should", "shall", "is", "are", "was", "were", "am", "do", "does", "did", "tell",
                "explain", "describe", "show", "please", "help",
            ];

            if question_starters.contains(&first_word) && !self.is_command_in_path(first_word) {
                return true;
            }
        }

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
    pub(super) fn is_path_execution(first_token: &str) -> bool {
        first_token.starts_with("./")
            || first_token.starts_with("../")
            || first_token.starts_with('/')
            || first_token.starts_with("~/")
    }

    /// 入力にシェル構文（パイプ、論理演算子、セミコロン、変数展開、代入）が含まれるか。
    pub(super) fn has_shell_syntax(input: &str) -> bool {
        input.contains('|')
            || input.contains("&&")
            || input.contains(';')
            || input.starts_with('$')
            || input.split_whitespace().any(|token| {
                token.contains('=') && token.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            })
    }
}
