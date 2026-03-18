//! reedline エディタの構築
//!
//! ハイライター、補完、キーバインディング、履歴、オートサジェストを設定した
//! reedline エディタを構築する。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use nu_ansi_term::{Color, Style};
use reedline::{
    default_emacs_keybindings, ColumnarMenu, DefaultHinter, Emacs, KeyCode, KeyModifiers,
    MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu,
};

use crate::cli::completer::JarvishCompleter;
use crate::cli::highlighter::JarvisHighlighter;
use crate::engine::classifier::InputClassifier;
use crate::storage::BlackBoxHistory;

/// ハイライター、補完、キーバインディング、履歴、オートサジェストを設定した
/// reedline エディタを構築する。
///
/// `db_path` は BlackBox と共有する `history.db` へのパス。
///
/// # Returns
/// `(Reedline, bool)` — `bool` はコマンド履歴の読み込みに成功したかどうか。
/// `false` の場合、矢印キー履歴とオートサジェスト（ヒンター）は無効。
pub fn build_editor(
    classifier: Arc<InputClassifier>,
    db_path: PathBuf,
    session_id: i64,
    git_branch_commands: Arc<RwLock<Vec<String>>>,
) -> (Reedline, bool) {
    let completer = Box::new(JarvishCompleter::new(git_branch_commands));
    let completion_menu = Box::new(ColumnarMenu::default().with_name("completion_menu"));

    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );

    let mut editor = Reedline::create()
        .with_highlighter(Box::new(JarvisHighlighter::new(classifier)))
        .with_completer(completer)
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
        .with_edit_mode(Box::new(Emacs::new(keybindings)));

    // コマンド履歴を BlackBox の SQLite テーブル (command_history) で管理。
    // DB オープンに失敗した場合は警告を出力し、履歴・ヒンターなしで動作を継続する。
    let history_available = match BlackBoxHistory::open(db_path, session_id) {
        Ok(history) => {
            let hinter = Box::new(
                DefaultHinter::default()
                    .with_style(Style::new().fg(Color::DarkGray))
                    .with_min_chars(2),
            );
            editor = editor.with_history(Box::new(history)).with_hinter(hinter);
            true
        }
        Err(e) => {
            eprintln!("jarvish: warning: failed to open history database: {e}");
            false
        }
    };

    (editor, history_available)
}
