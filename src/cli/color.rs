/// Iron Man テーマ: Red, Yellow, Cyan, White
///
/// ANSI カラーコードをラップするユーティリティ関数群。
/// 呼び出し側は RESET の付け忘れを気にする必要がない。

const RED: &str = "\x1b[91m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const WHITE: &str = "\x1b[97m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

pub fn red(text: &str) -> String {
    format!("{RED}{text}{RESET}")
}

pub fn yellow(text: &str) -> String {
    format!("{YELLOW}{text}{RESET}")
}

pub fn cyan(text: &str) -> String {
    format!("{CYAN}{text}{RESET}")
}

pub fn white(text: &str) -> String {
    format!("{WHITE}{text}{RESET}")
}

#[allow(dead_code)]
pub fn bold(text: &str) -> String {
    format!("{BOLD}{text}{RESET}")
}

pub fn bold_red(text: &str) -> String {
    format!("{BOLD}{RED}{text}{RESET}")
}

#[allow(dead_code)]
pub fn bold_yellow(text: &str) -> String {
    format!("{BOLD}{YELLOW}{text}{RESET}")
}

#[allow(dead_code)]
pub fn bold_white(text: &str) -> String {
    format!("{BOLD}{WHITE}{text}{RESET}")
}

#[allow(dead_code)]
pub fn bold_cyan(text: &str) -> String {
    format!("{BOLD}{CYAN}{text}{RESET}")
}
