use chrono::Local;
use rand::Rng;

use crate::color::{bold_red, cyan, white, yellow};
use crate::jarvis::jarvis_talk;

/// 時間帯に応じた挨拶を返す。
///  - 5〜11時:  "Good morning"
///  - 12〜17時: "Good afternoon"
///  - 18〜4時:  "Good evening"
fn time_greeting() -> &'static str {
    let hour = Local::now().hour();
    match hour {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    }
}

/// シェル起動時の Welcome バナーを表示する。
pub fn print_welcome() {
    let version = env!("CARGO_PKG_VERSION");
    let greeting = time_greeting();

    let art_lines: &[&str] = &[
        r#"   ___   ___   ______  _   _ _____  _____  _   _"#,
        r#"  |_  | / _ \  | ___ \| | | |_   _|/  ___|| | | |"#,
        r#"    | |/ /_\ \ | |_/ /| | | | | |  \ `--. | |_| |"#,
        r#"    | ||  _  | |    / | | | | | |   `--. \|  _  |"#,
        r#"/\__/ /| | | |_| |\ \ \ \_/ /_| |__/\__/ /| | | |_"#,
        r#"\____(_)_| |_(_)_| \_(_)___(_)___(_)____(_)_| |_(_)"#,
    ];

    let separator = "===================================================";
    let version_line = format!(
        "     {}  ::  {} {}",
        bold_red("J.A.R.V.I.S.H."),
        white("AI Native Shell"),
        yellow(&format!("v{version}"))
    );

    println!();
    for line in art_lines {
        println!("{}", white(line));
    }
    println!("{}", cyan(separator));
    println!("{}", yellow(&version_line));
    println!("{}", cyan(separator));
    println!();
    jarvis_talk(&format!("{greeting}, sir. All systems are operational."));
    println!();
}

/// シェル終了時の Farewell メッセージを表示する。
pub fn print_goodbye() {
    let greeting = time_greeting();

    let messages: &[&str] = &[
        "As always, sir, a great pleasure watching you work.",
        &format!("Powering down. {greeting}, sir."),
        &format!("Will that be all, sir? ... Enjoy your {greeting}."),
        "Until next time, sir. J.A.R.V.I.S.H. signing off.",
        "I'll keep the lights on for you, sir.",
    ];

    let idx = rand::rng().random_range(0..messages.len());

    println!();
    jarvis_talk(messages[idx]);
    println!();
}

/// `chrono::Timelike` を使うためのインポート（time_greeting 内で `.hour()` を呼ぶ）
use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_greeting_returns_valid_string() {
        let g = time_greeting();
        assert!(
            g == "Good morning" || g == "Good afternoon" || g == "Good evening",
            "unexpected greeting: {g}"
        );
    }
}
