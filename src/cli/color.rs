use nu_ansi_term::{Color, Style};

fn styled(color: Color, text: &str, is_bold: bool) -> String {
    let style = if is_bold {
        color.bold()
    } else {
        Style::new().fg(color)
    };
    style.paint(text).to_string()
}

pub fn red(text: &str) -> String {
    styled(Color::LightRed, text, false)
}

#[allow(dead_code)]
pub fn magenta(text: &str) -> String {
    styled(Color::Magenta, text, false)
}

pub fn green(text: &str) -> String {
    styled(Color::LightGreen, text, false)
}

pub fn yellow(text: &str) -> String {
    styled(Color::Yellow, text, false)
}

pub fn cyan(text: &str) -> String {
    styled(Color::Cyan, text, false)
}

pub fn white(text: &str) -> String {
    styled(Color::LightGray, text, false)
}

pub fn bold_red(text: &str) -> String {
    styled(Color::LightRed, text, true)
}

#[allow(dead_code)]
pub fn bold_magenta(text: &str) -> String {
    styled(Color::Magenta, text, true)
}

#[allow(dead_code)]
pub fn bold_green(text: &str) -> String {
    styled(Color::LightGreen, text, true)
}

#[allow(dead_code)]
pub fn bold_yellow(text: &str) -> String {
    styled(Color::Yellow, text, true)
}

#[allow(dead_code)]
pub fn bold_cyan(text: &str) -> String {
    styled(Color::Cyan, text, true)
}

#[allow(dead_code)]
pub fn bold_white(text: &str) -> String {
    styled(Color::LightGray, text, true)
}
