//! シェル展開機能
//!
//! - エイリアス展開 (`alias`): 先頭トークン置換
//! - コマンド置換 (`command_subst`): `$(...)` / backtick
//! - 基本展開 (`basic`): チルダ + 環境変数
//! - ブレース展開 (`brace`): `{a,b}` `{1..5}` 等
//! - グロブ展開 (`glob`): `*` `?` `[abc]`
//! - パイプライン (`pipeline`): command-subst → basic → brace → glob の統合 API
//!
//! 公開 API:
//! - [`expand_alias`] — 先頭トークンのエイリアス置換
//! - [`expand_token`] — チルダ/env のみ（1 出力）。`apply_exports` 等の単一値展開用
//! - [`expand_token_globs`] — command-subst + basic + brace + glob の統合（複数出力）。dispatch 用
//! - [`expand_token_globs_with_quoting`] — 上記のコマンド置換クォート文脈指定版
//! - [`ExpandError`] — グロブ no-match / コマンド置換失敗 等の展開失敗
//! - [`CmdSubstError`] / [`SubstQuoting`] — コマンド置換のエラー / クォート文脈

mod alias;
mod basic;
mod brace;
mod command_subst;
mod glob;
mod pipeline;
mod quote;

pub use alias::expand_alias;
pub use basic::expand_token;
pub use command_subst::{CmdSubstError, SubstQuoting};
pub use pipeline::{
    expand_token_globs, expand_token_globs_with_quoting, expand_token_subst_only, ExpandError,
};
pub(crate) use quote::operator_prefix_len;
pub use quote::{split_quoted, SplitError, Token};
