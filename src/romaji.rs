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

// ---- AZIK (拡張ローマ字入力) ----
//
// 「2 文字めに《ん》が来る」「二重母音」という日本語に多い並びを 2 打で入力する方式。
// 標準のローマ字入力を土台にしている。
//
// **綴りは標準ローマ字への展開として持つ。** `kz` → `kann` → 「かん」。かなを直に
// 書くと標準表と同じ知識を二重に持つことになり、片方だけ直して食い違う。

/// 撥音拡張と二重母音拡張。母音キーの代わりに打つと、母音の後ろが付いてくる。
///
/// 撥音は「その母音キーの下のキー」、二重母音は「近くのキー」という覚え方。
const AZIK_VOWELS: &[(char, &str)] = &[
    // 撥音拡張 (母音 + ん)
    ('z', "ann"),
    ('k', "inn"),
    ('j', "unn"),
    ('d', "enn"),
    ('l', "onn"),
    // 二重母音拡張
    ('q', "ai"),
    ('h', "uu"),
    ('w', "ei"),
    ('p', "ou"),
];

/// シャ行・チャ行の子音を 1 打で。
///
/// `sh` `ch` は二重母音拡張 (`s`+`h` = すう) に使うので、子音の方を別のキーへ移す。
/// シャ行は S の下の X、チャ行は Chocolate の C という覚え方。
const AZIK_CONSONANT_ALT: &[(char, &str)] = &[('x', "sy"), ('c', "ty")];

/// 小書きのかなは `l` を前置する。
///
/// 標準では `x` を前置するが、AZIK では `x` がシャ行の子音になるため。
const AZIK_SMALL: &[(&str, &str)] = &[
    ("la", "xa"),
    ("li", "xi"),
    ("lu", "xu"),
    ("le", "xe"),
    ("lo", "xo"),
    ("lya", "xya"),
    ("lyu", "xyu"),
    ("lyo", "xyo"),
    ("lwa", "xwa"),
    ("ltu", "xtu"),
];

/// 拗音を `y` の代わりに `g` で打てる行 (きゃ にゃ ひゃ みゃ ぴゃ)。
///
/// 左右の交互打鍵になる。3 打めには母音も拡張キーも使える (`kgp` = きょう)。
const AZIK_YOUON: &[char] = &['k', 'n', 'h', 'm', 'p'];

/// 「あ段の撥音」を `n` でも打てる子音。
///
/// 原則の `z` は左手の子音と続くと打ちにくいため (`sz` より `sn`)。
const AZIK_ANN_ALT: &[char] = &['d', 's', 'w', 'r', 'g', 'z', 't'];

/// 同じ指が続いて打ちにくい綴りを、母音の代わりに `f` で打つ。
const AZIK_SAME_FINGER: &[(&str, &str)] = &[
    ("kf", "ki"),
    ("jf", "ju"),
    ("hf", "hu"),
    ("yf", "yu"),
    ("mf", "mu"),
    ("nf", "nu"),
    ("df", "de"),
    ("cf", "tye"),
    ("pf", "ponn"),
];

/// 特殊拡張。撥音でも二重母音でもない頻出の並びを 2 打にする。
const AZIK_WORDS: &[(&str, &str)] = &[
    ("kt", "koto"),
    ("st", "sita"),
    ("tt", "tati"),
    ("ht", "hito"),
    ("wt", "wata"),
    ("mn", "mono"),
    ("ms", "masu"),
    ("ds", "desu"),
    ("km", "kamo"),
    ("tm", "tame"),
    ("dm", "demo"),
    ("kr", "kara"),
    ("sr", "suru"),
    ("tr", "tara"),
    ("nr", "naru"),
    ("yr", "yoru"),
    ("rr", "rare"),
    ("zr", "zaru"),
    ("mt", "mata"),
    ("tb", "tabi"),
    ("nb", "neba"),
    ("bt", "bito"),
    ("gr", "gara"),
    ("gt", "goto"),
    ("nt", "niti"),
    ("dt", "dati"),
    ("wr", "ware"),
];

/// 外来語向けと、原則どおりでは打ちにくいものの個別の綴り。
const AZIK_EXTRA: &[(&str, &str)] = &[
    ("wso", "who"),
    ("tgi", "thi"),
    ("dci", "dhi"),
    ("wf", "wai"),
    ("sf", "sai"),
    ("ss", "sei"),
    ("zc", "za"),
    ("zv", "zai"),
    ("zf", "ze"),
    ("zx", "zei"),
];

/// 標準ローマ字では書けない、かなを直に置くもの。
const AZIK_DIRECT: &[(&str, &str)] = &[
    // 単独の「ん」(nn でも打てる)
    ("q", "ん"),
    // 「っ」は常にこのキー
    (";", "っ"),
    // 長音記号
    (":", "ー"),
    ("wp", "うぉー"),
    ("fp", "ふぉー"),
];

/// AZIK の変換表。標準表から組み立てるので、起動時に一度だけ作る。
fn azik_table() -> &'static [(String, String)] {
    static CACHE: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    CACHE.get_or_init(build_azik).as_slice()
}

fn build_azik() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    // 個別に決めてあるものが先。あとから来る機械的な組み立てに負けないように。
    for (spell, kana) in AZIK_DIRECT {
        out.push((spell.to_string(), kana.to_string()));
    }
    let mut add = |spell: String, roman: &str| {
        if let Some(kana) = convert_standard(roman)
            && !out.iter().any(|(k, _)| *k == spell)
        {
            out.push((spell, kana));
        }
    };
    // シャ行・チャ行の子音キー
    for (key, cons) in AZIK_CONSONANT_ALT {
        for v in ["a", "i", "u", "e", "o"] {
            add(format!("{key}{v}"), &format!("{cons}{v}"));
        }
        for (ext, vowels) in AZIK_VOWELS {
            add(format!("{key}{ext}"), &format!("{cons}{vowels}"));
        }
    }
    for (spell, roman) in AZIK_SMALL {
        add(spell.to_string(), roman);
    }
    // 子音 + 拡張キー。子音は標準表の見出しから集めるので、書き漏らしが起きない。
    for cons in consonants() {
        for (key, vowels) in AZIK_VOWELS {
            add(format!("{cons}{key}"), &format!("{cons}{vowels}"));
        }
    }
    // 拗音の y を g で打つ。3 打めは母音でも拡張キーでもよい。
    for c in AZIK_YOUON {
        for v in ["a", "i", "u", "e", "o"] {
            add(format!("{c}g{v}"), &format!("{c}y{v}"));
        }
        for (key, vowels) in AZIK_VOWELS {
            add(format!("{c}g{key}"), &format!("{c}y{vowels}"));
        }
    }
    // あ段の撥音を n で
    for c in AZIK_ANN_ALT {
        add(format!("{c}n"), &format!("{c}ann"));
    }
    for (spell, roman) in AZIK_SAME_FINGER.iter().chain(AZIK_WORDS).chain(AZIK_EXTRA) {
        add(spell.to_string(), roman);
    }
    out
}

/// 標準表の見出しから、母音の前に来る部分 (子音の並び) を集める。
fn consonants() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (k, _, _) in TABLE {
        let head = k.trim_end_matches(['a', 'i', 'u', 'e', 'o']);
        if !head.is_empty() && head.len() < k.len() && !out.contains(&head) {
            out.push(head);
        }
    }
    out
}

/// 標準の表だけで最後まで変換できるなら、そのかなを返す。
///
/// 綴りの組み合わせを機械的に作るので、`yi` のようにかなにならないものが混ざる。
/// 最後まで変換できたかどうかで振り分ける。
fn convert_standard(roman: &str) -> Option<String> {
    let mut r = Romaji::new();
    let mut out = String::new();
    for c in roman.chars() {
        out.push_str(&r.feed(c));
    }
    let clean = r.is_empty() && !out.chars().any(|c| c.is_ascii_alphabetic());
    clean.then_some(out)
}

/// `.` と `,` から出す句読点の組。
///
/// 変換表そのものは「。、」を持ち、ここで指定された組へ差し替える。ddskk の
/// `skk-kutouten-type` と同じ四通り。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kutouten {
    /// 。 、 (既定)
    #[default]
    Jp,
    /// ． ，
    En,
    /// 。 ，
    JpEn,
    /// ． 、
    EnJp,
}

impl Kutouten {
    /// (句点, 読点)
    fn pair(self) -> (char, char) {
        match self {
            Kutouten::Jp => ('。', '、'),
            Kutouten::En => ('．', '，'),
            Kutouten::JpEn => ('。', '，'),
            Kutouten::EnJp => ('．', '、'),
        }
    }

    /// 変換表から出たかなの句読点を、この組へ差し替える。
    fn apply(self, s: String) -> String {
        if self == Kutouten::Jp {
            return s;
        }
        let (kuten, touten) = self.pair();
        s.chars()
            .map(|c| match c {
                '。' => kuten,
                '、' => touten,
                c => c,
            })
            .collect()
    }
}

/// ローマ字の入力途中を保持し、確定したかなを吐き出す。
#[derive(Default)]
pub struct Romaji {
    buf: String,
    kutouten: Kutouten,
    /// AZIK の拡張綴りも引くか。
    azik: bool,
}

impl Romaji {
    pub fn new() -> Self {
        Self::default()
    }

    /// 句読点の組を差し替える。設定の書き換えに追従するために使う。
    pub fn set_kutouten(&mut self, k: Kutouten) {
        self.kutouten = k;
    }

    /// AZIK (拡張ローマ字入力) を使うかどうか。
    pub fn set_azik(&mut self, on: bool) {
        self.azik = on;
    }

    /// いまの綴りで引く。AZIK が有効なら拡張綴りを先に見る。
    fn lookup(&self, key: &str) -> Option<(String, String)> {
        if self.azik
            && let Some((_, kana)) = azik_table().iter().find(|(k, _)| k == key)
        {
            return Some((kana.clone(), String::new()));
        }
        lookup(key).map(|(kana, rest)| (kana.to_string(), rest.to_string()))
    }

    /// この綴りに続きがあるか (まだ確定させてはいけないか)。
    fn has_prefix(&self, key: &str) -> bool {
        if self.azik
            && azik_table()
                .iter()
                .any(|(k, _)| k.len() > key.len() && k.starts_with(key))
        {
            return true;
        }
        has_prefix(key)
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
            if let Some((kana, rest)) = self.lookup(&self.buf) {
                out.push_str(&kana);
                self.buf = rest;
                continue;
            }
            if self.has_prefix(&self.buf) {
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
        self.kutouten.apply(out)
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

/// ひらがな・カタカナを半角カタカナにする。
///
/// 濁点・半濁点は独立した文字になるので、一文字が二文字に増える
/// (ガ → ｶﾞ)。半角の無い文字 (ヰ ヱ ヵ ヶ ヮ など) はそのまま残す。
pub fn to_hankaku_katakana(s: &str) -> String {
    to_katakana(s)
        .chars()
        .map(|c| {
            match c {
                'ア' => "ｱ",
                'イ' => "ｲ",
                'ウ' => "ｳ",
                'エ' => "ｴ",
                'オ' => "ｵ",
                'カ' => "ｶ",
                'キ' => "ｷ",
                'ク' => "ｸ",
                'ケ' => "ｹ",
                'コ' => "ｺ",
                'サ' => "ｻ",
                'シ' => "ｼ",
                'ス' => "ｽ",
                'セ' => "ｾ",
                'ソ' => "ｿ",
                'タ' => "ﾀ",
                'チ' => "ﾁ",
                'ツ' => "ﾂ",
                'テ' => "ﾃ",
                'ト' => "ﾄ",
                'ナ' => "ﾅ",
                'ニ' => "ﾆ",
                'ヌ' => "ﾇ",
                'ネ' => "ﾈ",
                'ノ' => "ﾉ",
                'ハ' => "ﾊ",
                'ヒ' => "ﾋ",
                'フ' => "ﾌ",
                'ヘ' => "ﾍ",
                'ホ' => "ﾎ",
                'マ' => "ﾏ",
                'ミ' => "ﾐ",
                'ム' => "ﾑ",
                'メ' => "ﾒ",
                'モ' => "ﾓ",
                'ヤ' => "ﾔ",
                'ユ' => "ﾕ",
                'ヨ' => "ﾖ",
                'ラ' => "ﾗ",
                'リ' => "ﾘ",
                'ル' => "ﾙ",
                'レ' => "ﾚ",
                'ロ' => "ﾛ",
                'ワ' => "ﾜ",
                'ヲ' => "ｦ",
                'ン' => "ﾝ",
                'ァ' => "ｧ",
                'ィ' => "ｨ",
                'ゥ' => "ｩ",
                'ェ' => "ｪ",
                'ォ' => "ｫ",
                'ッ' => "ｯ",
                'ャ' => "ｬ",
                'ュ' => "ｭ",
                'ョ' => "ｮ",
                'ガ' => "ｶﾞ",
                'ギ' => "ｷﾞ",
                'グ' => "ｸﾞ",
                'ゲ' => "ｹﾞ",
                'ゴ' => "ｺﾞ",
                'ザ' => "ｻﾞ",
                'ジ' => "ｼﾞ",
                'ズ' => "ｽﾞ",
                'ゼ' => "ｾﾞ",
                'ゾ' => "ｿﾞ",
                'ダ' => "ﾀﾞ",
                'ヂ' => "ﾁﾞ",
                'ヅ' => "ﾂﾞ",
                'デ' => "ﾃﾞ",
                'ド' => "ﾄﾞ",
                'バ' => "ﾊﾞ",
                'ビ' => "ﾋﾞ",
                'ブ' => "ﾌﾞ",
                'ベ' => "ﾍﾞ",
                'ボ' => "ﾎﾞ",
                'パ' => "ﾊﾟ",
                'ピ' => "ﾋﾟ",
                'プ' => "ﾌﾟ",
                'ペ' => "ﾍﾟ",
                'ポ' => "ﾎﾟ",
                'ヴ' => "ｳﾞ",
                'ー' => "ｰ",
                '、' => "､",
                '。' => "｡",
                '「' => "｢",
                '」' => "｣",
                '・' => "･",
                other => return other.to_string(),
            }
            .to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(r: &mut Romaji, s: &str) -> String {
        s.chars().map(|c| r.feed(c)).collect()
    }

    fn azik() -> Romaji {
        let mut r = Romaji::new();
        r.set_azik(true);
        r
    }

    /// 撥音拡張。母音キーの代わりに「その下のキー」で「母音 + ん」になる。
    #[test]
    fn azik_expands_the_syllabic_n() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "kz"), "かん");
        assert_eq!(typed(&mut r, "kk"), "きん");
        assert_eq!(typed(&mut r, "kj"), "くん");
        assert_eq!(typed(&mut r, "kd"), "けん");
        assert_eq!(typed(&mut r, "kl"), "こん");
        // 濁音・拗音の子音でも同じ
        assert_eq!(typed(&mut r, "gz"), "がん");
        assert_eq!(typed(&mut r, "kyz"), "きゃん");
        // シャ行・チャ行の子音は x と c (sh / ch は拡張に使うため)
        assert_eq!(typed(&mut r, "xz"), "しゃん");
        assert_eq!(typed(&mut r, "cz"), "ちゃん");
    }

    /// 二重母音拡張。
    #[test]
    fn azik_expands_the_double_vowels() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "sq"), "さい");
        assert_eq!(typed(&mut r, "kh"), "くう");
        assert_eq!(typed(&mut r, "sw"), "せい");
        assert_eq!(typed(&mut r, "kp"), "こう");
    }

    /// あ行には拡張を当てない。母音 + Q で「ん」を足す。
    #[test]
    fn azik_leaves_the_bare_vowels_alone() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "ai"), "あい");
        assert_eq!(typed(&mut r, "aq"), "あん");
        assert_eq!(typed(&mut r, "oq"), "おん");
    }

    /// 単打のキー。「っ」は常に `;`、長音は `:`、単独の「ん」は `q`。
    #[test]
    fn azik_single_strokes() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "a;ka:"), "あっかー");
        assert_eq!(typed(&mut r, "q"), "ん");
        assert_eq!(typed(&mut r, "nn"), "ん");
    }

    /// シャ行は X、チャ行は C。明け渡した `sh` `ch` は二重母音拡張になる。
    #[test]
    fn azik_sya_and_tya_move_to_x_and_c() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "xa"), "しゃ");
        assert_eq!(typed(&mut r, "xu"), "しゅ");
        assert_eq!(typed(&mut r, "xo"), "しょ");
        assert_eq!(typed(&mut r, "ca"), "ちゃ");
        assert_eq!(typed(&mut r, "cu"), "ちゅ");
        assert_eq!(typed(&mut r, "co"), "ちょ");
        assert_eq!(typed(&mut r, "sh"), "すう");
        assert_eq!(typed(&mut r, "ch"), "ちゅう");
    }

    /// 小書きのかなは l を前置する (x はシャ行に使ってしまったため)。
    #[test]
    fn azik_small_kana_with_l() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "la"), "ぁ");
        assert_eq!(typed(&mut r, "lyo"), "ょ");
        assert_eq!(typed(&mut r, "kulwa"), "くゎ");
    }

    /// 拗音の Y を G でも打てる (左右の交互打鍵になる)。3 打めは拡張キーでもよい。
    #[test]
    fn azik_youon_with_g() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "kga"), "きゃ");
        assert_eq!(typed(&mut r, "kgp"), "きょう");
        assert_eq!(typed(&mut r, "ngz"), "にゃん");
        // が行そのものは変わらない
        assert_eq!(typed(&mut r, "ga"), "が");
    }

    /// あ段の撥音は N でも打てる (左手の子音と続くと Z が打ちにくいため)。
    #[test]
    fn azik_ann_with_n() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "sn"), "さん");
        assert_eq!(typed(&mut r, "dn"), "だん");
        assert_eq!(typed(&mut r, "tn"), "たん");
    }

    /// 同じ指が続く綴りは F で打てる。
    #[test]
    fn azik_same_finger_keys() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "kf"), "き");
        assert_eq!(typed(&mut r, "mf"), "む");
        assert_eq!(typed(&mut r, "pf"), "ぽん");
    }

    /// 特殊拡張。頻出の並びを 2 打で。
    #[test]
    fn azik_special_words() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "kt"), "こと");
        assert_eq!(typed(&mut r, "ds"), "です");
        assert_eq!(typed(&mut r, "mn"), "もの");
        assert_eq!(typed(&mut r, "sr"), "する");
    }

    /// 外来語向けの綴り。
    #[test]
    fn azik_loanword_spellings() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "tgi"), "てぃ");
        assert_eq!(typed(&mut r, "dci"), "でぃ");
        assert_eq!(typed(&mut r, "wso"), "うぉ");
        assert_eq!(typed(&mut r, "wp"), "うぉー");
    }

    /// 標準のローマ字も打てる。ただし sh / ch はシャ行・チャ行の子音に譲ってある。
    #[test]
    fn azik_keeps_the_standard_spellings() {
        let mut r = azik();
        assert_eq!(typed(&mut r, "konnnitiha"), "こんにちは");
        assert_eq!(typed(&mut r, "kyou"), "きょう");
        assert_eq!(typed(&mut r, "si"), "し");
        assert_eq!(typed(&mut r, "ti"), "ち");
        // 子音の重ねは拡張と衝突するので、促音は ; を使う (tt は「たち」)
        assert_eq!(typed(&mut r, "tt"), "たち");
        assert_eq!(typed(&mut r, ";ta"), "った");
    }

    /// 切っていれば拡張は効かない。
    #[test]
    fn azik_is_off_by_default() {
        let mut r = Romaji::new();
        // kz は綴りにならないので、k がそのまま出て z が残る
        assert_eq!(typed(&mut r, "kz"), "k");
        assert_eq!(r.pending(), "z");
    }

    /// 句読点は設定した組で出る (ddskk の skk-kutouten-type と同じ四通り)。
    #[test]
    fn kutouten_follows_the_setting() {
        for (k, want) in [
            (Kutouten::Jp, "あ。い、"),
            (Kutouten::En, "あ．い，"),
            (Kutouten::JpEn, "あ。い，"),
            (Kutouten::EnJp, "あ．い、"),
        ] {
            let mut r = Romaji::new();
            r.set_kutouten(k);
            assert_eq!(typed(&mut r, "a.i,"), want, "{k:?}");
        }
    }

    /// 三点リーダなど、句読点を含まない記号は差し替えの対象外。
    #[test]
    fn kutouten_leaves_other_symbols_alone() {
        let mut r = Romaji::new();
        r.set_kutouten(Kutouten::En);
        assert_eq!(typed(&mut r, "z.z,"), "…‥");
    }

    /// 撥音と句点が一度に出るときも差し替わる。
    #[test]
    fn kutouten_applies_to_a_flushed_n() {
        let mut r = Romaji::new();
        r.set_kutouten(Kutouten::En);
        assert_eq!(typed(&mut r, "n."), "ん．");
    }

    #[test]
    fn hankaku_katakana() {
        assert_eq!(to_hankaku_katakana("にほんご"), "ﾆﾎﾝｺﾞ");
        assert_eq!(to_hankaku_katakana("パソコン"), "ﾊﾟｿｺﾝ");
        // 濁点・半濁点は独立した文字になるので一文字が二文字に増える
        assert_eq!(to_hankaku_katakana("が").chars().count(), 2);
        assert_eq!(to_hankaku_katakana("ぱ").chars().count(), 2);
        assert_eq!(to_hankaku_katakana("きゃっと"), "ｷｬｯﾄ");
        assert_eq!(to_hankaku_katakana("コーヒー"), "ｺｰﾋｰ");
        assert_eq!(to_hankaku_katakana("あ、い。"), "ｱ､ｲ｡");
        // 半角の無い文字はそのまま
        assert_eq!(to_hankaku_katakana("ヶ月"), "ヶ月");
        assert_eq!(to_hankaku_katakana("abc"), "abc");
    }

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
