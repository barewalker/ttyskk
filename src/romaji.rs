//! ローマ字からかなへの変換。
//!
//! 変換表は (入力, 出力かな, 残す入力) の三つ組。「残す入力」は "tt" → っ + "t" の
//! ような後続に持ち越す部分を表す。促音と撥音は表に全ての組を書かず、規則で処理する。

/// (ローマ字, ひらがな, 変換後に残る入力)
const TABLE: &[(&str, &str, &str)] = &[
    // 母音
    ("a", "あ", ""),
    ("i", "い", ""),
    ("u", "う", ""),
    ("e", "え", ""),
    ("o", "お", ""),
    // か行
    ("ka", "か", ""),
    ("ki", "き", ""),
    ("ku", "く", ""),
    ("ke", "け", ""),
    ("ko", "こ", ""),
    ("kya", "きゃ", ""),
    ("kyi", "きぃ", ""),
    ("kyu", "きゅ", ""),
    ("kye", "きぇ", ""),
    ("kyo", "きょ", ""),
    ("ga", "が", ""),
    ("gi", "ぎ", ""),
    ("gu", "ぐ", ""),
    ("ge", "げ", ""),
    ("go", "ご", ""),
    ("gya", "ぎゃ", ""),
    ("gyi", "ぎぃ", ""),
    ("gyu", "ぎゅ", ""),
    ("gye", "ぎぇ", ""),
    ("gyo", "ぎょ", ""),
    // さ行
    ("sa", "さ", ""),
    ("si", "し", ""),
    ("shi", "し", ""),
    ("su", "す", ""),
    ("se", "せ", ""),
    ("so", "そ", ""),
    ("sya", "しゃ", ""),
    ("syi", "しぃ", ""),
    ("syu", "しゅ", ""),
    ("sye", "しぇ", ""),
    ("syo", "しょ", ""),
    ("sha", "しゃ", ""),
    ("shu", "しゅ", ""),
    ("she", "しぇ", ""),
    ("sho", "しょ", ""),
    ("za", "ざ", ""),
    ("zi", "じ", ""),
    ("zu", "ず", ""),
    ("ze", "ぜ", ""),
    ("zo", "ぞ", ""),
    ("zya", "じゃ", ""),
    ("zyi", "じぃ", ""),
    ("zyu", "じゅ", ""),
    ("zye", "じぇ", ""),
    ("zyo", "じょ", ""),
    ("ja", "じゃ", ""),
    ("ji", "じ", ""),
    ("ju", "じゅ", ""),
    ("je", "じぇ", ""),
    ("jo", "じょ", ""),
    ("jya", "じゃ", ""),
    ("jyi", "じぃ", ""),
    ("jyu", "じゅ", ""),
    ("jye", "じぇ", ""),
    ("jyo", "じょ", ""),
    // た行
    ("ta", "た", ""),
    ("ti", "ち", ""),
    ("tu", "つ", ""),
    ("te", "て", ""),
    ("to", "と", ""),
    ("tya", "ちゃ", ""),
    ("tyi", "ちぃ", ""),
    ("tyu", "ちゅ", ""),
    ("tye", "ちぇ", ""),
    ("tyo", "ちょ", ""),
    ("chi", "ち", ""),
    ("cha", "ちゃ", ""),
    ("chu", "ちゅ", ""),
    ("che", "ちぇ", ""),
    ("cho", "ちょ", ""),
    ("tsu", "つ", ""),
    ("tsa", "つぁ", ""),
    ("tsi", "つぃ", ""),
    ("tse", "つぇ", ""),
    ("tso", "つぉ", ""),
    ("tha", "てゃ", ""),
    ("thi", "てぃ", ""),
    ("thu", "てゅ", ""),
    ("the", "てぇ", ""),
    ("tho", "てょ", ""),
    ("twu", "とぅ", ""),
    ("da", "だ", ""),
    ("di", "ぢ", ""),
    ("du", "づ", ""),
    ("de", "で", ""),
    ("do", "ど", ""),
    ("dya", "ぢゃ", ""),
    ("dyi", "ぢぃ", ""),
    ("dyu", "ぢゅ", ""),
    ("dye", "ぢぇ", ""),
    ("dyo", "ぢょ", ""),
    ("dha", "でゃ", ""),
    ("dhi", "でぃ", ""),
    ("dhu", "でゅ", ""),
    ("dhe", "でぇ", ""),
    ("dho", "でょ", ""),
    ("dwu", "どぅ", ""),
    // な行
    ("na", "な", ""),
    ("ni", "に", ""),
    ("nu", "ぬ", ""),
    ("ne", "ね", ""),
    ("no", "の", ""),
    ("nya", "にゃ", ""),
    ("nyi", "にぃ", ""),
    ("nyu", "にゅ", ""),
    ("nye", "にぇ", ""),
    ("nyo", "にょ", ""),
    ("nn", "ん", ""),
    ("n'", "ん", ""),
    // は行
    ("ha", "は", ""),
    ("hi", "ひ", ""),
    ("hu", "ふ", ""),
    ("he", "へ", ""),
    ("ho", "ほ", ""),
    ("hya", "ひゃ", ""),
    ("hyi", "ひぃ", ""),
    ("hyu", "ひゅ", ""),
    ("hye", "ひぇ", ""),
    ("hyo", "ひょ", ""),
    ("fu", "ふ", ""),
    ("fa", "ふぁ", ""),
    ("fi", "ふぃ", ""),
    ("fe", "ふぇ", ""),
    ("fo", "ふぉ", ""),
    ("fya", "ふゃ", ""),
    ("fyu", "ふゅ", ""),
    ("fyo", "ふょ", ""),
    ("ba", "ば", ""),
    ("bi", "び", ""),
    ("bu", "ぶ", ""),
    ("be", "べ", ""),
    ("bo", "ぼ", ""),
    ("bya", "びゃ", ""),
    ("byi", "びぃ", ""),
    ("byu", "びゅ", ""),
    ("bye", "びぇ", ""),
    ("byo", "びょ", ""),
    ("pa", "ぱ", ""),
    ("pi", "ぴ", ""),
    ("pu", "ぷ", ""),
    ("pe", "ぺ", ""),
    ("po", "ぽ", ""),
    ("pya", "ぴゃ", ""),
    ("pyi", "ぴぃ", ""),
    ("pyu", "ぴゅ", ""),
    ("pye", "ぴぇ", ""),
    ("pyo", "ぴょ", ""),
    // ま行
    ("ma", "ま", ""),
    ("mi", "み", ""),
    ("mu", "む", ""),
    ("me", "め", ""),
    ("mo", "も", ""),
    ("mya", "みゃ", ""),
    ("myi", "みぃ", ""),
    ("myu", "みゅ", ""),
    ("mye", "みぇ", ""),
    ("myo", "みょ", ""),
    // や行
    ("ya", "や", ""),
    ("yu", "ゆ", ""),
    ("ye", "いぇ", ""),
    ("yo", "よ", ""),
    // ら行
    ("ra", "ら", ""),
    ("ri", "り", ""),
    ("ru", "る", ""),
    ("re", "れ", ""),
    ("ro", "ろ", ""),
    ("rya", "りゃ", ""),
    ("ryi", "りぃ", ""),
    ("ryu", "りゅ", ""),
    ("rye", "りぇ", ""),
    ("ryo", "りょ", ""),
    // わ行
    ("wa", "わ", ""),
    ("wi", "ゐ", ""),
    ("we", "ゑ", ""),
    ("wo", "を", ""),
    ("wha", "うぁ", ""),
    ("whi", "うぃ", ""),
    ("whe", "うぇ", ""),
    ("who", "うぉ", ""),
    // ゔ
    ("va", "ゔぁ", ""),
    ("vi", "ゔぃ", ""),
    ("vu", "ゔ", ""),
    ("ve", "ゔぇ", ""),
    ("vo", "ゔぉ", ""),
    // 小書き (x 系。l 系は ASCII 切替キーと衝突するため置かない)
    ("xa", "ぁ", ""),
    ("xi", "ぃ", ""),
    ("xu", "ぅ", ""),
    ("xe", "ぇ", ""),
    ("xo", "ぉ", ""),
    ("xya", "ゃ", ""),
    ("xyu", "ゅ", ""),
    ("xyo", "ょ", ""),
    ("xtu", "っ", ""),
    ("xtsu", "っ", ""),
    ("xwa", "ゎ", ""),
    ("xka", "ヵ", ""),
    ("xke", "ヶ", ""),
    // 記号
    ("-", "ー", ""),
    (".", "。", ""),
    (",", "、", ""),
    ("[", "「", ""),
    ("]", "」", ""),
    ("z-", "〜", ""),
    ("z[", "『", ""),
    ("z]", "』", ""),
    ("z,", "‥", ""),
    ("z.", "…", ""),
    ("z/", "・", ""),
    ("zh", "←", ""),
    ("zj", "↓", ""),
    ("zk", "↑", ""),
    ("zl", "→", ""),
];

fn lookup(key: &str) -> Option<(&'static str, &'static str)> {
    TABLE
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, kana, rest)| (*kana, *rest))
}

fn has_prefix(key: &str) -> bool {
    TABLE
        .iter()
        .any(|(k, _, _)| k.len() > key.len() && k.starts_with(key))
}

fn is_consonant(c: char) -> bool {
    c.is_ascii_alphabetic() && !matches!(c.to_ascii_lowercase(), 'a' | 'i' | 'u' | 'e' | 'o')
}

/// ローマ字の入力途中を保持し、確定したかなを吐き出す。
#[derive(Default)]
pub struct Romaji {
    buf: String,
}

impl Romaji {
    pub fn new() -> Self {
        Self::default()
    }

    /// 未確定のローマ字。
    pub fn pending(&self) -> &str {
        &self.buf
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// 末尾を 1 文字消す。消す文字があれば true。
    pub fn backspace(&mut self) -> bool {
        self.buf.pop().is_some()
    }

    /// 1 文字与えて、確定した出力を返す。かなにならない文字はそのまま返る。
    pub fn feed(&mut self, c: char) -> String {
        self.buf.push(c);
        let mut out = String::new();
        loop {
            if self.buf.is_empty() {
                break;
            }
            if let Some((kana, rest)) = lookup(&self.buf) {
                out.push_str(kana);
                self.buf = rest.to_string();
                continue;
            }
            if has_prefix(&self.buf) {
                break;
            }
            let mut chars = self.buf.chars();
            let first = chars.next().unwrap();
            let second = chars.next();
            match second {
                // 促音: 同じ子音の重なり
                Some(s) if s == first && is_consonant(first) && first != 'n' => {
                    out.push('っ');
                    self.buf = self.buf[first.len_utf8()..].to_string();
                }
                // 撥音: n のあとにかなにならない文字が来た
                Some(_) if first == 'n' => {
                    out.push('ん');
                    self.buf = self.buf[1..].to_string();
                }
                // かなにならない先頭文字はそのまま出す
                Some(_) => {
                    out.push(first);
                    self.buf = self.buf[first.len_utf8()..].to_string();
                }
                None => {
                    out.push(first);
                    self.buf.clear();
                }
            }
        }
        out
    }

    /// 入力を打ち切る。"n" だけは「ん」として確定し、それ以外の半端な子音は捨てる。
    ///
    /// 残るのは必ず変換表の見出しの前半 (単独の子音など) で、それ自体は文字として
    /// 意味を持たない。子プロセスへ送ると余計な英字が紛れ込むので捨てる。
    pub fn flush(&mut self) -> String {
        let out = if self.buf == "n" { "ん" } else { "" };
        self.buf.clear();
        out.to_string()
    }
}

/// ひらがなをカタカナに変換する。
pub fn to_katakana(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'ぁ'..='ゖ' => char::from_u32(c as u32 + 0x60).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// カタカナをひらがなに変換する。
pub fn to_hiragana(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'ァ'..='ヶ' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// ASCII を全角に変換する。
pub fn to_zenkaku(c: char) -> char {
    match c {
        ' ' => '\u{3000}',
        '!'..='~' => char::from_u32(c as u32 - 0x21 + 0xff01).unwrap_or(c),
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(s: &str) -> String {
        let mut r = Romaji::new();
        let mut out = String::new();
        for c in s.chars() {
            out.push_str(&r.feed(c));
        }
        out.push_str(&r.flush());
        out
    }

    #[test]
    fn basic() {
        assert_eq!(conv("aiueo"), "あいうえお");
        assert_eq!(conv("kanji"), "かんじ");
        assert_eq!(conv("nihongo"), "にほんご");
    }

    #[test]
    fn sokuon() {
        assert_eq!(conv("gakkou"), "がっこう");
        assert_eq!(conv("itta"), "いった");
    }

    #[test]
    fn hatsuon() {
        assert_eq!(conv("nn"), "ん");
        assert_eq!(conv("hon"), "ほん");
        assert_eq!(conv("kanpeki"), "かんぺき");
        assert_eq!(conv("tanni"), "たんい");
        assert_eq!(conv("tannni"), "たんに");
    }

    #[test]
    fn youon_and_symbols() {
        assert_eq!(conv("syashin"), "しゃしん");
        assert_eq!(conv("cha-han"), "ちゃーはん");
        assert_eq!(conv("desu."), "です。");
    }

    #[test]
    fn katakana_roundtrip() {
        assert_eq!(to_katakana("あいうえお"), "アイウエオ");
        assert_eq!(to_hiragana("アイウエオ"), "あいうえお");
    }
}
