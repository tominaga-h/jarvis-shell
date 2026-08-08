use super::super::provider::Candidate;

/// `capture.zsh` の `zpty -w z "$*"$'\t'`（132行目、vendor 元のまま・
/// 改変不可）は argv をスペースで単純結合して内側 zsh の PTY バッファへ
/// 流し込む。そのため `ctx.spans()` の各要素（`git commit -m "hello world"`
/// の `"hello world"` のような、空白を含む 1 span）をそのまま argv として
/// 渡すと、内側 zsh 側では単純な空白区切りで 2 単語に分裂してしまい、
/// `$CURRENT`（内側 zsh から見たカーソル位置の単語インデックス）が
/// ずれて誤った/空の補完しか返らなくなる（実機検証済み: 5 spans のはずが
/// 内側 zsh では 6 words と数えられる）。
///
/// これを `capture.zsh` 側を一切変更せずに解決するため、Rust 側で各 span を
/// **zsh のバックスラッシュエスケープ規則で事前にエスケープしてから**
/// スペース結合する。空白・タブ・zsh の特殊文字をエスケープしておけば、
/// `"$*"` によるスペース結合後も内側 zsh のレキサがそれぞれを 1 単語として
/// 正しく再構成できる。
///
/// 注意: これは *送信方向*（Rust argv → 内側 zsh の `"$*"` バッファ）専用の
/// エスケープテーブルであり、*受信方向*（compadd 候補行 → Rust 側の表示値）
/// の [`unquote_backslashes`] とは独立している。両者はテーブルがほぼ重なる
/// が完全一致ではない（`=` は送信方向のみ）— 用途が異なるため往復対称性は
/// 意図的に要求しない（詳細は [`ZSH_SPECIAL_CHARS`] のドキュメント参照）。
///
/// 末尾の partial span（trailing space による新規単語補完のマーカーとして
/// 意図的に空文字列のまま渡される — `context.rs` の `spans()` 参照）は
/// **空のまま**（エスケープ不要かつ変形禁止）とする。空文字列をエスケープ
/// すると空文字列のままなので実質的には no-op だが、意図を明示するため
/// 明示的にスキップする分岐を設けている。
///
/// span に制御文字（`\n` `\r` `\0` などの C0 制御文字）が含まれる場合は
/// `None` を返し、呼び出し元（`provide()`）はこのプロバイダを丸ごと諦めて
/// `None` に縮退する。PTY への1行バッファ経由という `capture.zsh` の
/// プロトコル上、制御文字を安全に表現する手段がなく、無理にエスケープ
/// すると `capture.zsh` 内部のセンチネル行判定（NUL 行 = 応答区切り）を
/// 壊しうるため、安全側に倒して補完を諦める。
pub(crate) fn escape_spans(spans: &[String]) -> Option<Vec<String>> {
    spans.iter().map(|span| zsh_escape_span(span)).collect()
}

/// zsh の特殊文字集合（バックスラッシュエスケープ対象）。
///
/// 空白・タブに加え、`unquote_backslashes` の unquote テーブルに含まれる
/// 特殊文字全て（`\` `"` `'` `` ` `` `$` `|` `&` `;` `<` `>` `(` `)` `{` `}`
/// `[` `]` `*` `?` `~` `#` `=`）を対象にする。`=` は unquote テーブルには
/// 無いが、zsh のファイル名展開（`=command` 形式）を無効化するため追加で
/// エスケープする（余分なエスケープは `unquote_backslashes` 側で単に
/// そのまま復元されるだけなので副作用がない）。
pub(crate) const ZSH_SPECIAL_CHARS: &[char] = &[
    ' ', '\t', '\\', '"', '\'', '`', '$', '|', '&', ';', '<', '>', '(', ')', '{', '}', '[', ']',
    '*', '?', '~', '#', '=',
];

/// 1つの span を zsh のバックスラッシュエスケープ規則でエスケープする。
///
/// 空文字列（trailing partial のマーカー）はそのまま返す（no-op、意図的に
/// 変形しない — [`escape_spans`] のドキュメント参照）。制御文字（C0:
/// U+0000〜U+001F、および DEL の U+007F。`char::is_control` はこの範囲を
/// 過不足なく判定する）を含む場合は `None` — ただしタブ（U+0009）だけは
/// 例外で、`ZSH_SPECIAL_CHARS` に含まれるエスケープ対象文字として扱う
/// （1行バッファに安全に表現できない改行・復帰・NUL 等とは異なり、タブは
/// バックスラッシュエスケープすれば1行のまま安全に表現できるため）。
pub(crate) fn zsh_escape_span(span: &str) -> Option<String> {
    if span.is_empty() {
        return Some(String::new());
    }

    if span.chars().any(|c| c.is_control() && c != '\t') {
        return None;
    }

    let mut out = String::with_capacity(span.len());
    for ch in span.chars() {
        if ZSH_SPECIAL_CHARS.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    Some(out)
}

/// `capture.zsh` の stdout をパースして候補列に変換する。
///
/// PTY 由来の `\r\n` で分割し、末尾の空要素（トレイリング改行）は捨てる。
/// 各行は ANSI 除去 → バックスラッシュ unquote → 最初の `" -- "` で
/// value/description に分割、の順で処理する。
///
/// `pub(super)` なのは [`super::zsh_daemon::ZshDaemon`] が
/// 温存デーモンから読み取った候補行ブロック（NUL センチネル間、
/// `assets/zsh/daemon_init.zsh` の `compadd` オーバーライドが
/// `assets/zsh/capture.zsh` と同一の "value -- description" 形式で出力する）
/// をパースするために再利用するため。パースロジックの重複を避ける
/// （タスク指示: "Response parsing MUST reuse the existing zsh_bridge
/// parsing helpers"）。
pub(crate) fn parse_capture_output(stdout: &str) -> Vec<Candidate> {
    let mut lines: Vec<&str> = stdout.split("\r\n").collect();
    // 末尾の空要素（トレイリング区切りの結果）を捨てる。
    if lines.last() == Some(&"") {
        lines.pop();
    }

    lines
        .into_iter()
        .filter(|line| !line.is_empty())
        .filter_map(parse_capture_line)
        .collect()
}

/// 1 行を [`Candidate`] へ変換する。空行や value が空の行は `None`。
pub(crate) fn parse_capture_line(line: &str) -> Option<Candidate> {
    let stripped = strip_ansi(line);
    let unquoted = unquote_backslashes(&stripped);

    let (value, description) = match unquoted.find(" -- ") {
        Some(idx) => {
            let (value, rest) = unquoted.split_at(idx);
            // rest は " -- ..." なので " -- " (4 バイト) を飛ばす。
            (value.to_string(), Some(rest[4..].to_string()))
        }
        None => (unquoted, None),
    };

    if value.is_empty() {
        return None;
    }

    let append_whitespace = !ends_with_no_space_rune(&value);

    Some(Candidate {
        value,
        description,
        append_whitespace,
    })
}

/// carapace 慣習の「この文字で終わる値の後ろにはスペースを入れない」文字集合
/// （`carapace-bridge` の `NoSpace([]rune("/=@:.,"))` と同じ）。
pub(crate) fn ends_with_no_space_rune(value: &str) -> bool {
    matches!(
        value.chars().last(),
        Some('/' | '=' | '@' | ':' | '.' | ',')
    )
}

/// ANSI エスケープシーケンス（CSI: `ESC [ ... <final byte>`）を取り除く。
///
/// compsys の候補行は色付け（`zstyle ':completion:*' list-colors` 等）で
/// ANSI コードが混じることがあるため、確定挿入前に必ず取り除く。
pub(crate) fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
                          // パラメータ・中間バイトを読み飛ばし、final byte (0x40-0x7E) で終端。
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// `zsh.go` の unquoter テーブルと同じバックスラッシュエスケープを解除する。
///
/// 対象: `\\` `\&` `\<` `\>` `` \` `` `\'` `\"` `\{` `\}` `\$` `\#` `\|` `\?`
/// `\(` `\)` `\;` `\ ` `\[` `\]` `\*` `\~`
pub(crate) fn unquote_backslashes(input: &str) -> String {
    const ESCAPABLE: &[char] = &[
        '\\', '&', '<', '>', '`', '\'', '"', '{', '}', '$', '#', '|', '?', '(', ')', ';', ' ', '[',
        ']', '*', '~',
    ];

    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                if ESCAPABLE.contains(&next) {
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
        }
        out.push(ch);
    }
    out
}
