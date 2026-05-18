//! シェル展開機能
//!
//! - エイリアス展開 (`alias`): 先頭トークン置換
//! - 基本展開 (`basic`): チルダ + 環境変数
//! - ブレース展開 (`brace`): `{a,b}` `{1..5}` 等
//! - グロブ展開 (`glob`): `*` `?` `[abc]`
//! - パイプライン (`pipeline`): basic → brace → glob の統合 API
//!
//! 公開 API:
//! - [`expand_alias`] — 先頭トークンのエイリアス置換
//! - [`expand_token`] — チルダ/env のみ（1 出力）。`apply_exports` 等の単一値展開用
//! - [`expand_token_globs`] — basic + brace + glob の統合（複数出力）。dispatch 用
//! - [`ExpandError`] — グロブ no-match 等の展開失敗

mod alias;
mod basic;
mod brace;
mod glob;
mod pipeline;
mod quote;

pub use alias::expand_alias;
pub use basic::expand_token;
pub use pipeline::{expand_token_globs, ExpandError};
pub use quote::{split_quoted, SplitError, Token};
