//! reedline エディタの構築
//!
//! ハイライター、補完、キーバインディング、履歴、オートサジェストを設定した
//! reedline エディタを構築する。

use std::path::PathBuf;
use std::sync::Arc;

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
pub fn build_editor(classifier: Arc<InputClassifier>, db_path: PathBuf) -> Reedline {
    let completer = Box::new(JarvishCompleter::new(Arc::clone(&classifier)));
    let completion_menu = Box::new(ColumnarMenu::default().with_name("completion_menu"));

    // コマンド履歴を BlackBox の SQLite テーブル (command_history) で管理
    let history =
        Box::new(BlackBoxHistory::open(db_path).expect("failed to open history database"));

    // Fish ライクなオートサジェスト（履歴からグレーテキストで候補を表示）
    let hinter = Box::new(
        DefaultHinter::default()
            .with_style(Style::new().fg(Color::DarkGray))
            .with_min_chars(2),
    );

    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );

    Reedline::create()
        .with_history(history)
        .with_hinter(hinter)
        .with_highlighter(Box::new(JarvisHighlighter::new(classifier)))
        .with_completer(completer)
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
        .with_edit_mode(Box::new(Emacs::new(keybindings)))
}
