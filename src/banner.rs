use chrono::Local;
use rand::Rng;

// ANSI カラーコード
const RED: &str = "\x1b[91m";
const GOLD: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

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

    println!();
    println!(
        "  {BOLD}{RED}J.A.R.V.I.S.H.{RESET}  {GOLD}v{version}{RESET}"
    );
    println!(
        "  {CYAN}═══════════════════════════════════{RESET}"
    );
    println!();
    println!(
        "  {CYAN}{greeting}, sir. All systems are operational.{RESET}"
    );
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
    println!("  {CYAN}[J.A.R.V.I.S.] {}{RESET}", messages[idx]);
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
