use crate::engine::CommandResult;

/// Homebrew でインストールされているかを判定する。
pub(super) fn is_homebrew_install() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .map(|s| is_homebrew_path(&s))
        .unwrap_or(false)
}

/// パス文字列が Homebrew インストールのパスパターンに一致するか判定する。
///
/// Intel Mac: `/usr/local/Cellar/jarvish/...`
/// Apple Silicon: `/opt/homebrew/Cellar/jarvish/...`
pub(super) fn is_homebrew_path(exe_path: &str) -> bool {
    exe_path.contains("/Cellar/") || exe_path.contains("/homebrew/")
}

/// Homebrew インストールの場合の更新ハンドリング。
pub(super) fn handle_homebrew_update(check_only: bool) -> CommandResult {
    if check_only {
        let msg = "jarvish is installed via Homebrew.\n\
                   Run `brew outdated jarvish` to check for updates.\n";
        print!("{msg}");
        return CommandResult::success(msg.to_string());
    }

    let msg = "jarvish is installed via Homebrew.\n\
               Run `brew upgrade jarvish` to update, then `restart` to reload.\n";
    print!("{msg}");
    CommandResult::success(msg.to_string())
}
