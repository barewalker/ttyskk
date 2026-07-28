//! スニペット (定型文) の読み込み。
//!
//! 住所や電話番号、メールの署名のような「打つのが面倒で、内容が決まっている」
//! 文字列を、変換の候補として引けるようにする。辞書登録の画面で一文字ずつ打つ
//! 代わりに、**手元の編集器で一覧を見ながら書く**ためのもの。
//!
//! 書式は VS Code のスニペット (`*.code-snippets`) に合わせる。TextMate 由来の
//! この書式は LSP の仕様にも取り込まれていて、事実上の共通語になっている。
//! nvim の LuaSnip も `from_vscode` で同じファイルを読めるので、**編集器と
//! 入力メソッドで一つのファイルを分け合える**。
//!
//! ```jsonc
//! {
//!     // 会社の連絡先。異動があったらここだけ直す
//!     "会社住所 (日本語)": {
//!         "prefix": "かいしゃじゅうしょ",
//!         "body": ["東京都港区…"],
//!         "description": "日本語"
//!     },
//!     "メール署名": {
//!         "prefix": "しょめい",
//!         "body": ["竹内 光明", "株式会社レスカ"]
//!     }
//! }
//! ```
//!
//! `prefix` が見出し語、`body` が候補、`description` が注釈になる。`prefix` と
//! `body` はどちらも文字列でも文字列の並びでもよい (並びなら `prefix` は別々の
//! 見出し語、`body` は行として繋ぐ)。拡張子が `.jsonc` や `.code-snippets` の
//! ときと同じく、**注釈と末尾のカンマを許す** (VS Code もそう扱う)。

use anyhow::{Context, Result};

/// スニペット一つ。見出し語ごとに一つ作る。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    /// 見出し語 (かな)
    pub prefix: String,
    /// 候補の本文。行の並びは改行で繋いである。
    pub body: String,
    /// 候補に添える注釈
    pub description: Option<String>,
}

/// JSONC から注釈と末尾のカンマを落として素の JSON にする。
///
/// 落とした分は**空白に置き換える**。位置がずれないので、解析に失敗したときの
/// 行と桁がそのまま使える。文字列の中の `//` は注釈ではないので、文字列を
/// 跨ぐときは中身を触らない。
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                // 行末まで捨てる。改行は残す (行番号を保つ)
                out.push(' ');
                out.push(' ');
                chars.next();
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                    out.extend(std::iter::repeat_n(' ', c.len_utf8()));
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                out.push(' ');
                out.push(' ');
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    let done = prev == '*' && c == '/';
                    if c == '\n' {
                        out.push('\n');
                    } else {
                        out.extend(std::iter::repeat_n(' ', c.len_utf8()));
                    }
                    if done {
                        break;
                    }
                    prev = c;
                }
            }
            _ => out.push(c),
        }
    }
    drop_trailing_commas(&out)
}

/// `,` の次に `}` か `]` しか来ない場合、その `,` を空白にする。
fn drop_trailing_commas(text: &str) -> String {
    let bytes: Vec<char> = text.chars().collect();
    let mut out = bytes.clone();
    let mut in_string = false;
    let mut escaped = false;

    for i in 0..bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            ',' => {
                if let Some(next) = bytes[i + 1..].iter().find(|c| !c.is_whitespace())
                    && matches!(next, '}' | ']')
                {
                    out[i] = ' ';
                }
            }
            _ => {}
        }
    }
    out.into_iter().collect()
}

/// 文字列、または文字列の並びを取り出す。それ以外は空。
fn strings(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// スニペットの定義を読む。見出し語ごとに一つ返す。
///
/// 名前の付いた項目のうち `prefix` と `body` が揃っているものだけを拾う。
/// `prefix` が並びなら、同じ本文を引ける見出し語がその数だけできる。
pub fn parse(text: &str) -> Result<Vec<Snippet>> {
    let json = strip_jsonc(text);
    let root: serde_json::Value =
        serde_json::from_str(&json).context("スニペットの書式が JSON として読めない")?;
    let Some(table) = root.as_object() else {
        anyhow::bail!("スニペットの最も外側は {{ }} でなければならない");
    };

    let mut out = Vec::new();
    for (name, def) in table {
        let Some(def) = def.as_object() else { continue };
        let prefixes = def.get("prefix").map(strings).unwrap_or_default();
        let body = def.get("body").map(strings).unwrap_or_default().join("\n");
        if prefixes.is_empty() || body.is_empty() {
            continue;
        }
        // 注釈が無ければ項目の名前で代える。候補を選ぶときの手掛かりになる。
        let description = def
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| (!name.is_empty()).then(|| name.clone()));
        for prefix in prefixes {
            out.push(Snippet {
                prefix,
                body: body.clone(),
                description: description.clone(),
            });
        }
    }
    // ファイルに書いた順は保たれないので、見出し語で並べて毎回同じ順にする
    out.sort_by(|a, b| a.prefix.cmp(&b.prefix));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_vscode_shape() {
        let s = parse(
            r#"{
                "会社住所 (日本語)": {
                    "prefix": "かいしゃじゅうしょ",
                    "body": ["東京都港区…"],
                    "description": "日本語"
                },
                "会社住所 (英語)": {
                    "prefix": "かいしゃじゅうしょ",
                    "body": "1-2-3 …, Minato-ku, Tokyo",
                    "description": "英語"
                }
            }"#,
        )
        .unwrap();
        assert_eq!(s.len(), 2);
        // 同じ見出し語で二つの候補になる
        assert!(s.iter().all(|x| x.prefix == "かいしゃじゅうしょ"));
        let notes: Vec<&str> = s.iter().filter_map(|x| x.description.as_deref()).collect();
        assert!(notes.contains(&"日本語") && notes.contains(&"英語"));
    }

    /// 改行を含む定型文は `body` を行の並びで書く。SKK の辞書には書けない形。
    #[test]
    fn joins_the_body_lines_with_newlines() {
        let s =
            parse(r#"{"署名": {"prefix": "しょめい", "body": ["竹内 光明", "株式会社レスカ"]}}"#)
                .unwrap();
        assert_eq!(s[0].body, "竹内 光明\n株式会社レスカ");
        // 注釈が無ければ項目の名前を使う
        assert_eq!(s[0].description.as_deref(), Some("署名"));
    }

    /// 一つの定型文に複数の見出し語を付けられる。
    #[test]
    fn a_snippet_can_have_several_prefixes() {
        let s =
            parse(r#"{"電話": {"prefix": ["でんわ", "tel"], "body": "03-XXXX-XXXX"}}"#).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].prefix, "tel");
        assert_eq!(s[1].prefix, "でんわ");
        assert!(s.iter().all(|x| x.body == "03-XXXX-XXXX"));
    }

    /// 注釈と末尾のカンマを許す (VS Code の `.code-snippets` と同じ)。
    #[test]
    fn allows_comments_and_trailing_commas() {
        let s = parse(
            r#"{
                // 会社の連絡先。異動があったらここだけ直す
                "電話": {
                    "prefix": "でんわ",  /* 見出し語 */
                    "body": "03-XXXX-XXXX",
                },
            }"#,
        )
        .unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].body, "03-XXXX-XXXX");
    }

    /// 文字列の中の `//` は注釈ではない。URL を書いても壊れない。
    #[test]
    fn a_slash_inside_a_string_is_not_a_comment() {
        let s = parse(r#"{"社": {"prefix": "url", "body": "https://rhesca.co.jp/"}}"#).unwrap();
        assert_eq!(s[0].body, "https://rhesca.co.jp/");
    }

    /// 揃っていない項目は黙って飛ばす。書きかけでも他が使える。
    #[test]
    fn skips_incomplete_entries() {
        let s = parse(
            r#"{
                "書きかけ": {"prefix": "", "body": ""},
                "本文なし": {"prefix": "あ"},
                "使える": {"prefix": "い", "body": "居"}
            }"#,
        )
        .unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].prefix, "い");
    }

    #[test]
    fn reports_broken_json() {
        let e = parse(r#"{"壊れ": {"prefix": "あ" "body": "亜"}}"#).unwrap_err();
        assert!(format!("{e}").contains("読めない"), "{e}");
    }
}
