//! SKK の変換エンジン。端末にも GUI の入力メソッドにも載せられるように、
//! 画面まわりを含まない形で切り出してある。
//!
//! 使う側は [`skk::Skk`] にキーを一つずつ渡し、返ってきた [`skk::Response`] を
//! 出力に回す。入力中の表示は [`skk::Skk::preedit`]、候補の一覧は
//! [`skk::Skk::candidates`] で取り出す。
//!
//! ```no_run
//! use ttyskk::{config::Config, dict::Dict, skk::{Key, Skk}};
//!
//! # fn main() -> anyhow::Result<()> {
//! let dict = Dict::load(&[], "user.dict".into(), None)?;
//! let mut skk = Skk::new(dict, Config::default());
//! skk.handle(Key::Ctrl(0x0a)); // かなモードへ
//!
//! let r = skk.handle(Key::Char('a'));
//! assert_eq!(r.commit, "あ");            // 確定した文字列
//! assert_eq!(r.passthrough, None);       // 解釈しなかったキーは無い
//!
//! // 解釈しないキーはそのまま返ってくる。確定と同時に起きることもある。
//! let r = skk.handle(Key::Raw(b"\x1b[A".to_vec()));
//! assert_eq!(r.passthrough, Some(Key::Raw(b"\x1b[A".to_vec())));
//! # Ok(())
//! # }
//! ```
//!
//! 端末に載せる側 (擬似端末・重ね描き・バイト列の切り出し) は実行ファイルの方に
//! あり、この crate には入っていない。

pub mod config;
pub mod context;
pub mod dict;
pub mod migemo;
pub mod num;
pub mod romaji;
pub mod skk;
pub mod snippet;
