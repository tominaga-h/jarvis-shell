//! AI 応答の Goodbye 検出

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
        assert!(!is_ai_goodbye_response(""));
        assert!(!is_ai_goodbye_response(
            "Here is the command you need: ls -la"
        ));
        assert!(!is_ai_goodbye_response("エラーの原因はこちらです。"));
    }

    #[test]
    fn ai_goodbye_response_only_checks_tail() {
        let long_response = "Goodbye was mentioned here.\n\
                             Line 2\n\
                             Line 3\n\
                             Line 4\n\
                             Line 5\n\
                             This is just a normal response.";
        assert!(!is_ai_goodbye_response(long_response));
    }
}
