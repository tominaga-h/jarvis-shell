use chrono::Local;
use rand::Rng;

use super::color::{red, yellow};
use super::jarvis::jarvis_talk;

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
///
/// `offline_systems` にオフラインのサブシステム名を渡すと、
/// "All systems are operational." の代わりに状態を報告する。
pub fn print_welcome(offline_systems: &[&str]) {
    let version = env!("CARGO_PKG_VERSION");
    let greeting = time_greeting();

    let art_lines: &[&str] = &[
        r#"   _   _   ___ _   _ ___ ___ _  _ "#,
        r#"  | | /_\ | _ \ \ / /_ _/ __| || |"#,
        r#" _| |/ _ \|   /\ V / | |\__ \ __ |"#,
        r#" \__/_/ \_\_|_\ \_/ |___|___/_||_|"#,
    ];

    // ASCII ロゴの幅に合わせた細線セパレータの右端にバージョンを置く。
    let art_width = art_lines.iter().map(|l| l.len()).max().unwrap_or(0);
    let version_tag = format!("v{version}");
    let dash_len = art_width.saturating_sub(version_tag.len()) - 1;
    let separator = format!("{} {}", "─".repeat(dash_len), yellow(&version_tag));

    println!();
    for line in art_lines {
        println!("{}", red(line));
    }
    println!("{separator}");
    println!();

    if offline_systems.is_empty() {
        jarvis_talk(&format!("{greeting}, sir. All systems are operational."));
    } else {
        let detail = offline_systems.join(", ");
        jarvis_talk(&format!(
            "{greeting}, sir. Partially operational — {detail}."
        ));
    }

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
