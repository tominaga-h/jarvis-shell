//! reedline エディタの構築
//!
//! ハイライター、補完、キーバインディングを設定した reedline エディタを構築する。

use std::sync::Arc;

use reedline::{
    default_emacs_keybindings, ColumnarMenu, Emacs, KeyCode, KeyModifiers, MenuBuilder, Reedline,
    ReedlineEvent, ReedlineMenu,
};

use crate::cli::completer::JarvishCompleter;
use crate::cli::highlighter::JarvisHighlighter;
use crate::engine::classifier::InputClassifier;

/// ハイライター、補完、キーバインディングを設定した reedline エディタを構築する。
pub fn build_editor(classifier: Arc<InputClassifier>) -> Reedline {
    let completer = Box::new(JarvishCompleter::new());
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

    Reedline::create()
        .with_highlighter(Box::new(JarvisHighlighter::new(classifier)))
        .with_completer(completer)
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
        .with_edit_mode(Box::new(Emacs::new(keybindings)))
}
