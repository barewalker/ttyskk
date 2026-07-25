//! 数値変換。見出し語の数字を `#` に置き換えて辞書を引き、候補の `#N` を戻す。
//!
//! 「だい5かい」で変換すると、見出し語は `だい#かい` になる。`SKK-JISYO.L` には
//! `だい# /第#1/第#0/` のような項目が 490 個あり、これが無いとどれも引けない。
//! `#` に続く一桁が変換の型を表す。
//!
//! | 型 | 例 (1234) | 用途 |
//! |---|---|---|
//! | `#0` | 1234 | そのまま |
//! | `#1` | １２３４ | 全角数字 |
//! | `#2` | 一二三四 | 漢数字 (位取りなし) |
//! | `#3` | 千二百三十四 | 漢数字 (位取りあり) |
//! | `#5` | 壱阡弐百参拾四 | 大字 |
//! | `#8` | 1,234 | 桁区切り |
//! | `#9` | (2 桁のみ) | 将棋の棋譜 |
//!
//! `#4` (数値再変換) は `SKK-JISYO.L` に 1 件しかなく、辞書を再帰的に引く必要が
//! あるため扱わない。そのままの数字を出す。

/// 位取りに使う文字の組。漢数字と大字で入れ替える。
struct Numerals {
    digits: [&'static str; 10],
    /// 十・百・千
    units: [&'static str; 4],
    /// 万・億・兆・京
    big: [&'static str; 5],
    /// 位の頭の「一」を省くか (漢数字は省き、大字は残す)
    omit_one: bool,
}

const KANSUJI: Numerals = Numerals {
    digits: ["", "一", "二", "三", "四", "五", "六", "七", "八", "九"],
    units: ["", "十", "百", "千"],
    big: ["", "万", "億", "兆", "京"],
    omit_one: true,
};

const DAIJI: Numerals = Numerals {
    digits: ["", "壱", "弐", "参", "四", "五", "六", "七", "八", "九"],
    units: ["", "拾", "百", "阡"],
    big: ["", "萬", "億", "兆", "京"],
    omit_one: false,
};

/// 見出し語の数字を `#` に置き換え、取り出した数字を順に返す。
///
/// 「だい5かい」→ (`だい#かい`, `["5"]`)。数字が無ければ元のまま返る。
pub fn abstract_numbers(reading: &str) -> (String, Vec<String>) {
    let mut key = String::with_capacity(reading.len());
    let mut nums = Vec::new();
    let mut run = String::new();
    for c in reading.chars() {
        if c.is_ascii_digit() {
            run.push(c);
            continue;
        }
        if !run.is_empty() {
            key.push('#');
            nums.push(std::mem::take(&mut run));
        }
        key.push(c);
    }
    if !run.is_empty() {
        key.push('#');
        nums.push(run);
    }
    (key, nums)
}

/// 候補の `#N` を、取り出した数字で置き換える。
///
/// `#` は現れた順に数字を使う。数字が足りなければその印は元のまま残す。
pub fn expand(text: &str, nums: &[String]) -> String {
    if nums.is_empty() || !text.contains('#') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut used = 0;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c != '#' {
            out.push(c);
            continue;
        }
        let Some(&t) = it.peek().filter(|t| t.is_ascii_digit()) else {
            out.push('#');
            continue;
        };
        let Some(n) = nums.get(used) else {
            out.push('#');
            continue;
        };
        it.next();
        used += 1;
        out.push_str(&convert(n, t as u8 - b'0'));
    }
    out
}

/// 数字ひとつを指定の型に変える。
fn convert(n: &str, kind: u8) -> String {
    match kind {
        1 => n.chars().map(zenkaku_digit).collect(),
        2 => n.chars().map(kansuji_digit).collect(),
        3 => positional(n, &KANSUJI),
        5 => positional(n, &DAIJI),
        8 => grouped(n),
        9 => shogi(n),
        // 0 と、扱わない 4 はそのまま
        _ => n.to_string(),
    }
}

fn zenkaku_digit(c: char) -> char {
    if c.is_ascii_digit() {
        char::from_u32(c as u32 - '0' as u32 + '０' as u32).unwrap_or(c)
    } else {
        c
    }
}

fn kansuji_digit(c: char) -> char {
    match c {
        '0' => '〇',
        '1' => '一',
        '2' => '二',
        '3' => '三',
        '4' => '四',
        '5' => '五',
        '6' => '六',
        '7' => '七',
        '8' => '八',
        '9' => '九',
        _ => c,
    }
}

/// 位取りのある表記。4 桁ずつ区切り、万・億・兆で繋ぐ。
fn positional(n: &str, t: &Numerals) -> String {
    let ds: Vec<u8> = n.bytes().filter(|b| b.is_ascii_digit()).collect();
    let ds: Vec<u8> = ds.iter().map(|b| b - b'0').collect();
    let start = ds.iter().position(|&d| d != 0).unwrap_or(ds.len());
    let ds = &ds[start..];
    if ds.is_empty() {
        return if t.omit_one {
            "〇".into()
        } else {
            "零".into()
        };
    }
    // 京より上は位取りで表せないので、一文字ずつに落とす
    if ds.len() > t.big.len() * 4 {
        return n.chars().map(kansuji_digit).collect();
    }

    let mut groups: Vec<&[u8]> = Vec::new();
    let mut rest = ds;
    while !rest.is_empty() {
        let cut = rest.len().saturating_sub(4);
        groups.push(&rest[cut..]);
        rest = &rest[..cut];
    }
    // groups[0] が一の位側。大きい位から並べ直す。
    let mut out = String::new();
    for (i, g) in groups.iter().enumerate().rev() {
        let body = group4(g, t);
        if body.is_empty() {
            continue;
        }
        out.push_str(&body);
        out.push_str(t.big[i]);
    }
    out
}

/// 4 桁までを千・百・十で表す。
fn group4(g: &[u8], t: &Numerals) -> String {
    let mut out = String::new();
    let len = g.len();
    for (i, &d) in g.iter().enumerate() {
        if d == 0 {
            continue;
        }
        let unit = len - 1 - i; // 0 = 一の位
        // 位の頭の「一」は漢数字では省く (千, 百, 十)。大字では残す。
        if !(d == 1 && unit > 0 && t.omit_one) {
            out.push_str(t.digits[d as usize]);
        }
        out.push_str(t.units[unit]);
    }
    out
}

/// 3 桁ごとに `,` を挟む。
fn grouped(n: &str) -> String {
    let ds: Vec<char> = n.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut out = String::new();
    for (i, c) in ds.iter().enumerate() {
        if i > 0 && (ds.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

/// 将棋の棋譜。2 桁を「全角数字 + 漢数字」にする (34 → ３四)。
fn shogi(n: &str) -> String {
    let ds: Vec<char> = n.chars().collect();
    if ds.len() != 2 {
        return n.to_string();
    }
    format!("{}{}", zenkaku_digit(ds[0]), kansuji_digit(ds[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nums(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pulls_the_numbers_out_of_the_reading() {
        assert_eq!(
            abstract_numbers("だい5かい"),
            ("だい#かい".into(), nums(&["5"]))
        );
        assert_eq!(
            abstract_numbers("3かい2ばん"),
            ("#かい#ばん".into(), nums(&["3", "2"]))
        );
        assert_eq!(abstract_numbers("1234"), ("#".into(), nums(&["1234"])));
        assert_eq!(abstract_numbers("かんじ"), ("かんじ".into(), vec![]));
    }

    #[test]
    fn converts_each_type() {
        let n = nums(&["1234"]);
        assert_eq!(expand("#0", &n), "1234");
        assert_eq!(expand("#1", &n), "１２３４");
        assert_eq!(expand("#2", &n), "一二三四");
        assert_eq!(expand("#3", &n), "千二百三十四");
        assert_eq!(expand("#5", &n), "壱阡弐百参拾四");
        assert_eq!(expand("#8", &n), "1,234");
        // 扱わない #4 はそのまま
        assert_eq!(expand("#4", &n), "1234");
    }

    #[test]
    fn positional_handles_the_awkward_places() {
        let t = |s: &str| expand("#3", &nums(&[s]));
        assert_eq!(t("1"), "一");
        assert_eq!(t("10"), "十", "位の頭の一は省く");
        assert_eq!(t("11"), "十一");
        assert_eq!(t("100"), "百");
        assert_eq!(t("1000"), "千");
        assert_eq!(t("10000"), "一万", "万の前の一は残す");
        assert_eq!(t("100000000"), "一億");
        assert_eq!(t("10001"), "一万一");
        assert_eq!(t("1020304"), "百二万三百四");
        assert_eq!(t("0"), "〇");
        assert_eq!(t("007"), "七", "先頭の零は落とす");
        // 大字は位の頭の一を残す
        assert_eq!(expand("#5", &nums(&["10"])), "壱拾");
    }

    #[test]
    fn substitutes_in_order_and_leaves_the_rest() {
        assert_eq!(expand("#1かい#2ばん", &nums(&["3", "5"])), "３かい五ばん");
        // 数字が足りなければ印を残す
        assert_eq!(expand("#1と#1", &nums(&["3"])), "３と#1");
        // 型でない `#` はそのまま
        assert_eq!(expand("C#と#1", &nums(&["3"])), "C#と３");
        // 数字が無ければ何もしない
        assert_eq!(expand("第#1回", &[]), "第#1回");
    }

    #[test]
    fn shogi_and_grouping() {
        assert_eq!(expand("#9", &nums(&["34"])), "３四");
        assert_eq!(expand("#9", &nums(&["345"])), "345", "2 桁以外はそのまま");
        assert_eq!(expand("#8", &nums(&["1234567"])), "1,234,567");
        assert_eq!(expand("#8", &nums(&["123"])), "123");
    }
}
