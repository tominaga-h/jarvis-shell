/// フラグファイルのパス: `<data_dir>/update-ready`
pub(super) fn update_flag_path() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("", "", "jarvish").map(|p| p.data_dir().join("update-ready"))
}

/// 更新完了後にフラグファイルを作成して兄弟プロセスに通知する。
///
/// フラグファイルには新しいバージョン番号を書き込む。
/// 兄弟プロセスは次のプロンプト表示時にこのファイルを検出し、
/// ユーザーに `restart` コマンドの実行を促す。
/// テスト用にフラグファイルを書き込む公開ヘルパー。
#[cfg(test)]
pub fn write_update_flag_for_test(version: &str) {
    write_update_flag(version);
}

pub(super) fn write_update_flag(version: &str) {
    let Some(path) = update_flag_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, version);
}

/// フラグファイルを確認し、更新通知メッセージを返す。
///
/// フラグファイルが存在する場合は読み取って削除し、通知文字列を返す。
/// REPL ループのプロンプト表示前に呼び出される。
pub fn check_update_flag() -> Option<String> {
    let path = update_flag_path()?;
    let version = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let version = version.trim();
    if version.is_empty() {
        return None;
    }
    Some(format!(
        "jarvish has been updated to v{version}. Run `restart` to apply."
    ))
}
