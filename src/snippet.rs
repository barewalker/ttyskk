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

/// いまの日時。定型文の変数を開くのに使う。
///
/// **エンジンの側では日時を読まない。** 地方時に直すには libc が要り、この層は
/// 端末にも GUI にも載せられるよう libc を持たない作りにしてある。呼ぶ側が
/// 教える ([`crate::skk::Skk::set_now`])。教えなければ変数は開かず、書いたままの
/// 姿で出る。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Now {
    pub year: i32,
    /// 1〜12
    pub month: u32,
    /// 1〜31
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = 日曜
    pub weekday: u32,
    /// Unix 時刻 (秒)
    pub unix: i64,
}

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

impl Now {
    /// 変数名に対する値。知らない名前なら `None`。
    ///
    /// 名前は TextMate (VS Code・LSP) のものに合わせる。日付と時刻だけを扱い、
    /// ファイル名や選択範囲のように端末で意味を持たないものは持たない。
    fn value(&self, name: &str) -> Option<String> {
        let month = (self.month.clamp(1, 12) - 1) as usize;
        let weekday = (self.weekday % 7) as usize;
        Some(match name {
            "CURRENT_YEAR" => format!("{:04}", self.year),
            "CURRENT_YEAR_SHORT" => format!("{:02}", self.year.rem_euclid(100)),
            "CURRENT_MONTH" => format!("{:02}", self.month),
            "CURRENT_MONTH_NAME" => MONTH_NAMES[month].to_string(),
            "CURRENT_MONTH_NAME_SHORT" => MONTH_NAMES[month][..3].to_string(),
            "CURRENT_DATE" => format!("{:02}", self.day),
            "CURRENT_DAY_NAME" => DAY_NAMES[weekday].to_string(),
            "CURRENT_DAY_NAME_SHORT" => DAY_NAMES[weekday][..3].to_string(),
            "CURRENT_HOUR" => format!("{:02}", self.hour),
            "CURRENT_MINUTE" => format!("{:02}", self.minute),
            "CURRENT_SECOND" => format!("{:02}", self.second),
            "CURRENT_SECONDS_UNIX" => self.unix.to_string(),
            _ => return None,
        })
    }
}

/// 定型文の中の変数を、いまの値に置き換える。
///
/// `$CURRENT_YEAR` と `${CURRENT_YEAR}` のどちらの書き方も通る。**知らない名前は
/// そのまま残す** — `$100` のような普通の文字列を勝手に消さないため。`\$` と
/// 書けば、変数として読まずに `$` そのものになる。
pub fn expand_variables(text: &str, now: &Now) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && chars.get(i + 1) == Some(&'$') {
            out.push('$');
            i += 2;
            continue;
        }
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // `${NAME}` と `$NAME` の両方を見る
        let (name_start, braced) = match chars.get(i + 1) {
            Some('{') => (i + 2, true),
            _ => (i + 1, false),
        };
        let mut end = name_start;
        while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        let name: String = chars[name_start..end].iter().collect();
        let closed = !braced || chars.get(end) == Some(&'}');
        match now.value(&name).filter(|_| closed) {
            Some(v) => {
                out.push_str(&v);
                i = if braced { end + 1 } else { end };
            }
            // 知らない名前や閉じていない括弧は、書いたまま残す
            None => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

/// 何も無いところに置く最初の中身。
///
/// 空のファイルを編集器で開いても書き出しに困るので、書き方をその場に置いておく。
pub const TEMPLATE: &str = r#"{
    // ttyskk の定型文。書式は VS Code のスニペット (*.code-snippets)。
    //
    //   prefix      … 見出し語 (かな)。並びにすると複数の見出し語で引ける
    //   body        … 候補。並びにすると行として繋がる (改行を含む定型文)
    //   description … 候補の右に出る注釈。日本語版と英語版の区別などに
    //
    // 注釈 (この行のような //) と末尾のカンマを書いてよい。保存した時点で
    // ttyskk が読み直すので、起動し直さなくてよい。
}
"#;

/// JSON の文字列に入れられる形にする。
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// まだ使われていない項目名を作る。
///
/// 項目名は JSON の鍵なので、同じものが二度あると後の一つしか残らない。見出し語を
/// 種にして、既にあれば番号を足す。
fn unique_name(text: &str, prefix: &str) -> String {
    let base = if prefix.is_empty() {
        "新しい定型文"
    } else {
        prefix
    };
    let used = |name: &str| text.contains(&format!("\"{}\"", escape(name)));
    if !used(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !used(name))
        .expect("番号はいくらでもある")
}

/// 新しい項目の雛形を末尾へ足す。足したあと、書き始める行を返す (1 から数える)。
///
/// 編集器はその行にカーソルを置いて開く。**書き足す場所を探すところから始めずに
/// 済ませる**ためのもので、一覧を見ながら書くという狙いの半分はここにある。
///
/// 見出し語が分かっていれば `prefix` に埋め、カーソルは本文へ置く。分からなければ
/// 見出し語の側へ置く。
pub fn append_template(text: &str, prefix: &str) -> (String, usize) {
    let base = if text.trim().is_empty() {
        TEMPLATE.to_string()
    } else {
        text.to_string()
    };

    // 最も外側の閉じ括弧を探す。そこより前が中身。
    let Some(close) = base.rfind('}') else {
        // 括弧が無いなら壊れている。触らずに先頭を指す。
        return (base, 1);
    };
    let head = &base[..close];
    let tail = &base[close..];

    // 直前の項目にカンマが無ければ足す (末尾のカンマは許されるので付けたままでよい)。
    // **注釈を外してから見る** — 「…よい。」で終わる注釈の後ろにカンマは要らない。
    let needs_comma = strip_jsonc(head)
        .trim_end()
        .chars()
        .next_back()
        .is_some_and(|c| c != ',' && c != '{');
    let comma = if needs_comma { "," } else { "" };

    // 項目の名前は JSON の鍵なので、**同じ名前を二度書くと前のものが消える**。
    // 空のまま足すと二つめで一つめを失うため、必ず被らない名前にする。
    let escaped = escape(prefix);
    let entry = format!(
        "{comma}\n    \"{}\": {{\n        \"prefix\": \"{escaped}\",\n        \"body\": [\"\"],\n        \"description\": \"\"\n    }},\n",
        escape(&unique_name(&base, prefix))
    );

    let out = format!("{}{}{}", head.trim_end(), entry, tail);
    // 書き始める行。足した項目の最後の行 (`},`) から数え上げる。
    // 見出し語が入っていれば本文へ、空なら見出し語へ置く。
    let last = out[..out.len() - tail.len()].lines().count();
    let line = if prefix.is_empty() {
        last - 3 // "prefix" の行
    } else {
        last - 2 // "body" の行
    };
    (out, line)
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

    fn now() -> Now {
        // 2026-07-28 (火) 13:05:07
        Now {
            year: 2026,
            month: 7,
            day: 28,
            hour: 13,
            minute: 5,
            second: 7,
            weekday: 2,
            unix: 1_785_243_907,
        }
    }

    /// 日付や時刻を書いておくと、使うときの値に開く。
    #[test]
    fn expands_the_date_variables() {
        let n = now();
        assert_eq!(expand_variables("$CURRENT_YEAR", &n), "2026");
        assert_eq!(
            expand_variables("${CURRENT_YEAR}-${CURRENT_MONTH}-${CURRENT_DATE}", &n),
            "2026-07-28"
        );
        assert_eq!(
            expand_variables("$CURRENT_HOUR:$CURRENT_MINUTE", &n),
            "13:05"
        );
        assert_eq!(expand_variables("$CURRENT_DAY_NAME_SHORT", &n), "Tue");
        assert_eq!(expand_variables("$CURRENT_MONTH_NAME", &n), "July");
        assert_eq!(expand_variables("$CURRENT_YEAR_SHORT", &n), "26");
    }

    /// 知らない名前は書いたまま残す。金額や変数名を勝手に消さない。
    #[test]
    fn leaves_unknown_variables_alone() {
        let n = now();
        assert_eq!(expand_variables("$100 と $200", &n), "$100 と $200");
        assert_eq!(expand_variables("$HOME/bin", &n), "$HOME/bin");
        assert_eq!(expand_variables("${CURRENT_YEAR", &n), "${CURRENT_YEAR");
        // \$ と書けば $ そのもの
        assert_eq!(expand_variables("\\$CURRENT_YEAR", &n), "$CURRENT_YEAR");
    }

    /// 日時を教えられていなければ (既定の Now)、年は 0000 になるだけで壊れない。
    #[test]
    fn works_without_a_clock() {
        let n = Now::default();
        assert_eq!(expand_variables("$CURRENT_YEAR", &n), "0000");
        assert_eq!(expand_variables("ふつうの文字列", &n), "ふつうの文字列");
    }

    /// 何も無いところに足すと、書き方の見本ごと出来上がる。
    #[test]
    fn creates_the_first_entry_from_nothing() {
        let (text, line) = append_template("", "でんわ");
        // 足した直後のものが読める形になっている (雛形の中身は空なので 0 件)
        assert_eq!(parse(&text).unwrap().len(), 0);
        // 見出し語は埋まっている
        assert!(text.contains(r#""prefix": "でんわ""#), "{text}");
        // カーソルは本文の行
        assert_eq!(
            text.lines().nth(line - 1).unwrap().trim(),
            r#""body": [""],"#
        );
    }

    /// 既にある項目の後ろへ足す。前の項目にカンマが無くても壊さない。
    #[test]
    fn appends_after_an_existing_entry() {
        let before = "{\n    \"電話\": {\n        \"prefix\": \"でんわ\",\n        \"body\": \"03\"\n    }\n}\n";
        let (text, line) = append_template(before, "");
        let got = parse(&text).unwrap();
        assert_eq!(got.len(), 1, "元の項目は残る: {text}");
        assert_eq!(got[0].prefix, "でんわ");
        // カーソルは見出し語の行 (見出し語が決まっていないので)
        assert_eq!(
            text.lines().nth(line - 1).unwrap().trim(),
            r#""prefix": "","#
        );
    }

    /// 末尾にカンマがある書き方でも二重にしない。
    #[test]
    fn does_not_double_the_comma() {
        let before = "{\n    \"電話\": {\n        \"prefix\": \"でんわ\",\n        \"body\": \"03\"\n    },\n}\n";
        let (text, _) = append_template(before, "しょめい");
        assert!(!text.contains(",,"), "{text}");
        assert_eq!(parse(&text).unwrap().len(), 1);
    }

    /// 見出し語に " が入っていても壊れない。
    #[test]
    fn escapes_the_prefix() {
        let (text, _) = append_template("", "a\"b");
        assert!(parse(&text).is_ok(), "{text}");
    }

    /// 続けて足しても前のものが消えない。
    ///
    /// 項目の名前は JSON の鍵なので、同じ名前で二度書くと前の一つが失われる。
    #[test]
    fn adding_twice_keeps_both() {
        let (one, _) = append_template("", "でんわ");
        let filled = one.replace(r#""body": [""]"#, r#""body": ["03"]"#);
        let (two, _) = append_template(&filled, "しょめい");
        let two = two.replace(r#""body": [""]"#, r#""body": ["竹内"]"#);

        let got = parse(&two).unwrap();
        assert_eq!(got.len(), 2, "両方残るはず: {two}");
        let prefixes: Vec<&str> = got.iter().map(|s| s.prefix.as_str()).collect();
        assert!(prefixes.contains(&"でんわ") && prefixes.contains(&"しょめい"));

        // 同じ見出し語で足しても消えない (名前に番号が付く)
        let (three, _) = append_template(&two, "でんわ");
        let three = three.replace(r#""body": [""]"#, r#""body": ["+81"]"#);
        assert_eq!(parse(&three).unwrap().len(), 3, "{three}");
    }

    #[test]
    fn reports_broken_json() {
        let e = parse(r#"{"壊れ": {"prefix": "あ" "body": "亜"}}"#).unwrap_err();
        assert!(format!("{e}").contains("読めない"), "{e}");
    }
}
