//! ローマ字から日本語を探す正規表現を作る。
//!
//! `ttyskk migemo kaigi` が `(?:kaigi|かいぎ|カイギ|会議|回議|懐疑|…)` を吐く。
//! **ローマ字→かな変換ではない。** 読みから見出し語を引いて**漢字にも当てる**ところが要で、
//! そのための辞書は SKK-JISYO をそのまま使う (migemo の辞書はもともとここから作られている)。
//!
//! # 呼ばれ方
//!
//! Neovim の絞り込み (fzf のプロンプトや `/` 検索) から、利用者が入力を確定した時に
//! 一度だけ呼ばれる。打鍵ごとではないので、単発の起動で足りる。
//!
//! # 方言
//!
//! 出来上がりを食わせる先が Vim と ripgrep の二つあり、**群と or の書き方が違う**。
//! 片方だけ作ると内容検索だけが静かに何も見つけなくなるので、[`Rxop`] で最初から
//! 分けてある。

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::dict::Dict;
use crate::romaji::{self, Romaji};

/// 読みから見出し語を引くもの。
///
/// 共有辞書をそのまま読む道 ([`Dict`]) と、先に作っておいた索引を読む道 ([`Index`])
/// の二つがある。組み立ての側はどちらでも同じように使う。
pub trait Headwords {
    /// 読みぴったりの見出し語。
    fn lookup(&self, key: &str) -> Vec<String>;
    /// その読みで始まる、より長い読み。短いものから順に `limit` 件まで。
    fn complete(&self, prefix: &str, limit: usize) -> Vec<String>;
}

impl Headwords for Dict {
    fn lookup(&self, key: &str) -> Vec<String> {
        Dict::lookup(self, key)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    fn complete(&self, prefix: &str, limit: usize) -> Vec<String> {
        Dict::complete(self, prefix, limit)
    }
}

/// 正規表現の方言。
///
/// 値は kensaku.vim の `g:kensaku#rxop#vim` と `#javascript` に揃えてある。
#[derive(Clone, Copy)]
pub struct Rxop {
    /// 全体の先頭に一度だけ置くもの。
    pub prefix: &'static str,
    pub or: &'static str,
    pub start_group: &'static str,
    pub end_group: &'static str,
    pub start_class: &'static str,
    pub end_class: &'static str,
    /// この方言で特別な意味を持つ文字。候補に現れたら `\` を前置する。
    pub escape: &'static str,
}

/// Vim の正規表現 (`magic`)。
pub const VIM: Rxop = Rxop {
    prefix: r"\m",
    or: r"\|",
    start_group: r"\%(",
    end_group: r"\)",
    start_class: "[",
    end_class: "]",
    escape: r"\.[]*~^$",
};

/// ripgrep (Rust の regex crate)。
pub const RG: Rxop = Rxop {
    prefix: "",
    or: "|",
    start_group: "(?:",
    end_group: ")",
    start_class: "[",
    end_class: "]",
    escape: r"\.[]{}()*+-?^$|",
};

/// 名前から方言を引く。
pub fn flavour(name: &str) -> Option<Rxop> {
    match name {
        "vim" => Some(VIM),
        "rg" => Some(RG),
        _ => None,
    }
}

/// 一つの語に集める見出し語の数の上限。
///
/// 一文字を打った時が最も膨らむ。**時間ではなく長さのための上限** — 実測では
/// 上限を 100 から 2000 まで動かしても所要時間は変わらず (辞書を読む分が大きい)、
/// 出来上がりの長さだけが 279 バイトから 1795 バイトまで伸びた。
///
/// 1000 は `a` 一文字で 1.2 kB ほど。kensaku の同じ入力 (1483 文字) と釣り合う長さで、
/// Vim にも ripgrep の `--regex-size-limit` にも余裕がある。
pub const DEFAULT_LIMIT: usize = 1000;

// ---- 索引 ----
//
// 共有辞書をそのまま読むと、EUC-JP の変換と 17 万件の組み立てで 200 ms を超える。
// 絞り込みは打ち終わるたびに走るので、ここが体感のほとんどを占めてしまう。
//
// **常駐はしない。** それは denops と同じ構造に戻ることになる。代わりに、読みと
// 見出し語だけを UTF-8 の平らな表に落としておき、必要な範囲だけ二分探索で拾う。

/// 索引の一行目に置く印。形を変えたら数字を上げる。
const INDEX_MAGIC: &str = "ttyskk-migemo-index\t1";

/// 事前に作っておく索引。
///
/// 中身は `読み\t見出し語\t見出し語…` を読み順に並べたもの。**利用者辞書は入れない** —
/// 覚えるたびに作り直すことになるので、そちらは毎回読む (数千語なので直ぐ済む)。
pub struct Index {
    text: String,
    /// 本文の各行。`text` の中の範囲で持つ。
    lines: Vec<Range<usize>>,
    /// 利用者辞書と定型文。索引と重ねて引く。
    user: Dict,
}

/// 元の辞書が入れ替わっていないかを見るための印。
fn stamp(path: &Path) -> String {
    let m = std::fs::metadata(path).ok();
    let size = m.as_ref().map_or(0, |m| m.len());
    let secs = m
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    format!("{}:{secs}:{size}", path.display())
}

fn header(sources: &[PathBuf]) -> String {
    let stamps: Vec<String> = sources.iter().map(|p| stamp(p)).collect();
    format!("{INDEX_MAGIC}\t{}", stamps.join("\t"))
}

impl Index {
    /// 索引の中身を作る。
    pub fn build(dict: &Dict, sources: &[PathBuf]) -> String {
        // 二分探索で引くので、読み順に並べておく。
        let mut entries: Vec<(&str, &[crate::dict::Candidate])> = dict
            .system_entries()
            // 数値変換の見出しは、そのままでは文字列として当たらない。
            .filter(|(k, _)| !k.contains('#'))
            .collect();
        entries.sort_unstable_by_key(|(k, _)| *k);

        let mut out = header(sources);
        out.push('\n');
        for (key, cands) in entries {
            out.push_str(key);
            for c in cands {
                // 注釈は要らない。改行とタブが混じると行の形が壊れるので落とす。
                out.push('\t');
                out.push_str(&c.text.replace(['\t', '\n'], ""));
            }
            out.push('\n');
        }
        out
    }

    /// 索引を読む。
    ///
    /// 無いときと、元の辞書が入れ替わっているときは `None`。**どちらも誤りではない** —
    /// 呼ぶ側は辞書をそのまま読む道へ回る (遅いだけで、答えは変わらない)。
    pub fn load(path: &Path, sources: &[PathBuf], user: Dict) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("{} を読めない", path.display()))?;
        let Some((first, _)) = text.split_once('\n') else {
            return Ok(None);
        };
        if first != header(sources) {
            return Ok(None);
        }

        let start = first.len() + 1;
        let mut lines = Vec::new();
        let mut at = start;
        for line in text[start..].split('\n') {
            if !line.is_empty() {
                lines.push(at..at + line.len());
            }
            at += line.len() + 1;
        }
        Ok(Some(Index { text, lines, user }))
    }

    /// 索引を書き出す。
    pub fn save(path: &Path, body: &str) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("{} を作れない", dir.display()))?;
        }
        std::fs::write(path, body).with_context(|| format!("{} を書けない", path.display()))
    }

    fn line(&self, i: usize) -> &str {
        &self.text[self.lines[i].clone()]
    }

    fn key_of(&self, i: usize) -> &str {
        let line = self.line(i);
        line.split('\t').next().unwrap_or(line)
    }

    /// 読みが `key` 以上になる最初の行。
    fn lower_bound(&self, key: &str) -> usize {
        let (mut lo, mut hi) = (0, self.lines.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.key_of(mid) < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// `prefix` で始まる行の範囲。
    fn prefix_range(&self, prefix: &str) -> Range<usize> {
        let lo = self.lower_bound(prefix);
        let mut hi = lo;
        while hi < self.lines.len() && self.key_of(hi).starts_with(prefix) {
            hi += 1;
        }
        lo..hi
    }
}

impl Headwords for Index {
    fn lookup(&self, key: &str) -> Vec<String> {
        let mut out = Headwords::lookup(&self.user, key);
        let i = self.lower_bound(key);
        if i < self.lines.len() && self.key_of(i) == key {
            for text in self.line(i).split('\t').skip(1) {
                if !out.iter().any(|e| e == text) {
                    out.push(text.to_string());
                }
            }
        }
        out
    }

    fn complete(&self, prefix: &str, limit: usize) -> Vec<String> {
        let mut out = Headwords::complete(&self.user, prefix, limit);
        let mut rest: Vec<&str> = self
            .prefix_range(prefix)
            .map(|i| self.key_of(i))
            // 英字だけの見出しは索引にあるが、前方一致では引かない (辞書をそのまま
            // 読む道では送りあり扱いで索引から外れており、揃えないと答えが変わる)。
            .filter(|k| k.len() > prefix.len() && !k.is_ascii())
            .collect();
        // 辞書と同じ並び (短いものから、同じ長さなら辞書順) に揃える。
        rest.sort_by(|a, b| a.chars().count().cmp(&b.chars().count()).then(a.cmp(b)));
        for k in rest {
            if out.len() >= limit {
                break;
            }
            if !out.iter().any(|e| e == k) {
                out.push(k.to_string());
            }
        }
        out
    }
}

/// 索引の置き場所。
pub fn index_path() -> PathBuf {
    if let Some(p) = std::env::var_os("TTYSKK_MIGEMO_INDEX") {
        return PathBuf::from(p);
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
            home.join(".cache")
        });
    cache.join("ttyskk/migemo.index")
}

/// 引く相手を用意する。索引があればそれを、無ければ辞書をそのまま読む。
///
/// **索引が無くても動く。** 遅いだけで答えは変わらないので、黙って辞書を読む。
pub fn source(sources: &[PathBuf], user_path: PathBuf) -> Result<Box<dyn Headwords>> {
    if !sources.iter().any(|p| p.exists()) {
        let places: Vec<String> = sources.iter().map(|p| p.display().to_string()).collect();
        bail!(
            "共有辞書が無い ({})。TTYSKK_JISYO で場所を指せる",
            places.join(", ")
        );
    }
    // 索引を使う時は、共有辞書を読まない分だけ速い。利用者辞書は毎回読む。
    let user = Dict::load(&[], user_path.clone(), None)?;
    if let Some(ix) = Index::load(&index_path(), sources, user)? {
        return Ok(Box::new(ix));
    }
    Ok(Box::new(Dict::load(sources, user_path, None)?))
}

/// 正規表現を組み立てる。
///
/// 空白は語の区切りで、語ごとの群を繋げたものになる。`azik` は設定の
/// `romaji.azik` をそのまま渡す — 打つ人の綴りで読みを作らないと、拡張綴りを
/// 使っている入力が当たらない。
pub fn build(query: &str, rx: &Rxop, dict: &dyn Headwords, limit: usize, azik: bool) -> String {
    let mut out = String::from(rx.prefix);
    for word in query.split_whitespace() {
        out.push_str(&word_regex(word, rx, dict, limit, azik));
    }
    out
}

/// 語ひとつ分の群。
fn word_regex(word: &str, rx: &Rxop, dict: &dyn Headwords, limit: usize, azik: bool) -> String {
    let mut root = Node::default();
    for c in candidates(word, dict, limit, azik) {
        root.insert(&c);
    }
    // 語の群は**常に**包む。隣の語と繋げた時に or が漏れないようにするため。
    // 内側の emit は要る時しか包まないので、ここで二重にはならない。
    let alts = alternatives(&root, rx);
    format!("{}{}{}", rx.start_group, alts.join(rx.or), rx.end_group)
}

/// 語ひとつ分の候補を、当たってほしい順に並べて返す。
///
/// 並びは 入力そのもの → ひらがな → カタカナ → 半角カナ → 見出し語 → 全角英数。
/// 出来上がりの並びは木が決める (文字の順) ので、ここの順序は上限で切る時の
/// 優先順位として効く。
fn candidates(word: &str, dict: &dyn Headwords, limit: usize, azik: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };

    // 1. 打った字面そのもの。大小はそのまま残す。
    push(word.to_string());

    // 2. かな。**読みは大小を保たない** ので、引く前に小さくする。
    let readings = readings(word, azik);
    for r in &readings {
        push(r.clone());
        push(romaji::to_katakana(r));
        push(romaji::to_hankaku_katakana(r));
    }

    // 3. 全角英数。字面の大小はここでも保つ。
    push(word.chars().map(romaji::to_zenkaku).collect());

    // 4. 読みで引いた見出し語。数が読めないのでここだけ上限を置く。
    for text in headwords(word, &readings, dict, limit) {
        push(text);
    }
    out
}

/// 読みで見出し語を引く。読みぴったりと、その読みで始まるものの両方。
///
/// 打った字面でも一度引く。SKK-JISYO には英字の見出し (`a /エー/エイ/`、
/// `note /ノート/`) があり、これは読みからは辿り着けない。**前方一致では引けない** —
/// 末尾が英字の見出しは送りありと見分けが付かないので、索引から外してある。
fn headwords(word: &str, readings: &[String], dict: &dyn Headwords, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let lower = word.to_lowercase();
    for key in [word, lower.as_str()] {
        for text in dict.lookup(key) {
            if !out.contains(&text) {
                out.push(text);
            }
        }
    }
    if readings.is_empty() {
        out.truncate(limit);
        return out;
    }
    // 読みが複数ある (半端なローマ字を展開した) 時に、先頭の読みだけで
    // 上限を使い切らないよう分け合う。
    let each = limit.div_ceil(readings.len());
    for r in readings {
        let mut taken = 0;
        for key in std::iter::once(r.clone()).chain(dict.complete(r, each)) {
            // 数値変換の見出し (`だい#`) は、そのままでは文字列として当たらない。
            if key.contains('#') {
                continue;
            }
            for text in dict.lookup(&key) {
                if !out.contains(&text) {
                    out.push(text);
                    taken += 1;
                }
            }
            if taken >= each {
                break;
            }
        }
    }
    out.truncate(limit);
    out
}

/// ローマ字から読みを作る。半端な綴りは続きうるかなに展開する。
///
/// `kaigi` は `かいぎ` の一つだが、`kaig` は `かいが` `かいぎ` … `かいっ` に広がる。
/// 打っている途中で呼ばれるので、ここを畳むと絞り込みが効かなくなる。
fn readings(word: &str, azik: bool) -> Vec<String> {
    let (kana, pending) = to_kana(&word.to_lowercase(), azik);
    if pending.is_empty() {
        return if kana.is_empty() {
            Vec::new()
        } else {
            vec![kana]
        };
    }
    expand(&pending, azik)
        .into_iter()
        .map(|tail| format!("{kana}{tail}"))
        .collect()
}

/// ローマ字を流し込んで、確定したかなと未確定の綴りに分ける。
fn to_kana(word: &str, azik: bool) -> (String, String) {
    let mut r = Romaji::new();
    r.set_azik(azik);
    let mut out = String::new();
    for c in word.chars() {
        out.push_str(&r.feed(c));
    }
    (out, r.pending().to_string())
}

/// 半端な綴りに続きうるかな。
///
/// **表を写さない。** 母音と、同じ字の重なり (促音) を実際に流し込んでみて、
/// かなになったものを拾う。AZIK の拡張綴りも同じ道を通るので取りこぼさない。
fn expand(pending: &str, azik: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(last) = pending.chars().next_back() else {
        return out;
    };
    for tail in ['a', 'i', 'u', 'e', 'o', last] {
        let (kana, rest) = to_kana(&format!("{pending}{tail}"), azik);
        // 促音は「っ」を出して子音を持ち越すので、綴りが残っていても拾う。
        if !kana.is_empty() && !out.contains(&kana) && (rest.is_empty() || tail == last) {
            out.push(kana);
        }
    }
    out
}

/// 候補を字で辿れるように並べたもの。共通の接頭辞をまとめるために使う。
#[derive(Default)]
struct Node {
    children: BTreeMap<char, Node>,
    /// ここで終わる候補がある。
    terminal: bool,
}

impl Node {
    /// 候補を一つ加える。
    ///
    /// **短い候補があれば長い方は要らない。** 前方一致で当たるので、`ノート` を
    /// 入れた後の `ノートPC` は木に足さない (足すと長くなるだけで何も増えない)。
    fn insert(&mut self, s: &str) {
        if self.terminal {
            return;
        }
        let mut chars = s.chars();
        match chars.next() {
            None => {
                self.terminal = true;
                self.children.clear();
            }
            Some(c) => self.children.entry(c).or_default().insert(chars.as_str()),
        }
    }
}

/// ここから先に進む道を、選択肢の並びとして返す。
///
/// 一字で終わる枝は文字クラスに集め、先頭に置く (`怪(?:[訝魚]|現象)` の形)。
/// `海技` と `海魚` が `海[技魚]` に、`買玉` と `買い玉` が `買(?:玉|い玉)` になる。
fn alternatives(node: &Node, rx: &Rxop) -> Vec<String> {
    let mut singles = String::new();
    let mut single_count = 0;
    let mut branches: Vec<String> = Vec::new();
    for (c, child) in &node.children {
        let rest = emit(child, rx);
        if rest.is_empty() {
            singles.push_str(&escape(*c, rx));
            single_count += 1;
        } else {
            branches.push(format!("{}{rest}", escape(*c, rx)));
        }
    }

    let mut alts: Vec<String> = Vec::new();
    match single_count {
        0 => {}
        // 一字だけならクラスにする意味がない
        1 => alts.push(singles),
        _ => alts.push(format!("{}{singles}{}", rx.start_class, rx.end_class)),
    }
    alts.append(&mut branches);
    alts
}

/// 木を正規表現にする。分かれ道がある時だけ群で包む。
fn emit(node: &Node, rx: &Rxop) -> String {
    let mut alts = alternatives(node, rx);
    match alts.len() {
        0 => String::new(),
        1 => alts.remove(0),
        _ => format!("{}{}{}", rx.start_group, alts.join(rx.or), rx.end_group),
    }
}

/// その方言で特別な意味を持つ文字を逃がす。
fn escape(c: char, rx: &Rxop) -> String {
    if rx.escape.contains(c) {
        format!("\\{c}")
    } else {
        c.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 試験用の辞書を一つ作る。置き場所 (共有辞書のパスと、その入れ物) も返す。
    fn dict_at(entries: &[(&str, &str)]) -> (Dict, PathBuf, PathBuf) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ttyskk-migemo-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = dir.join("sys.dict");
        let mut body = String::from(";; okuri-nasi entries.\n");
        for (k, v) in entries {
            body.push_str(&format!("{k} {v}\n"));
        }
        std::fs::write(&sys, body).unwrap();
        let d = Dict::load(std::slice::from_ref(&sys), dir.join("user.dict"), None).unwrap();
        (d, sys, dir)
    }

    fn dict(entries: &[(&str, &str)]) -> Dict {
        dict_at(entries).0
    }

    /// 木に入れて正規表現にする。辞書を通さずに形だけ見る。
    fn compress(cands: &[&str], rx: &Rxop) -> String {
        let mut root = Node::default();
        for c in cands {
            root.insert(c);
        }
        emit(&root, rx)
    }

    /// 共通の接頭辞をまとめる。
    #[test]
    fn a_shared_prefix_is_folded() {
        // 一字で分かれるならクラス
        assert_eq!(compress(&["海技", "海魚"], &RG), "海[技魚]");
        // 長さが違えば群
        assert_eq!(compress(&["買玉", "買い玉"], &RG), "買(?:玉|い玉)");
        // クラスと群が混ざる時はクラスが先
        assert_eq!(
            compress(&["怪訝", "怪魚", "怪現象"], &RG),
            "怪(?:[訝魚]|現象)"
        );
        // 分かれ道が無ければ何も足さない
        assert_eq!(compress(&["会議"], &RG), "会議");
    }

    /// 短い候補があれば、それで始まる長い候補は要らない。
    #[test]
    fn a_longer_candidate_under_a_shorter_one_is_dropped() {
        // どちらの順で入れても同じ
        assert_eq!(compress(&["ノート", "ノートPC"], &RG), "ノート");
        assert_eq!(compress(&["ノートPC", "ノート"], &RG), "ノート");
    }

    /// 方言で群と or の書き方が変わる。
    #[test]
    fn both_flavours_come_out() {
        let d = dict(&[("かいぎ", "/会議/")]);
        assert_eq!(
            build("kaigi", &RG, &d, DEFAULT_LIMIT, false),
            "(?:kaigi|かいぎ|カイギ|会議|ｋａｉｇｉ|ｶｲｷﾞ)"
        );
        assert_eq!(
            build("kaigi", &VIM, &d, DEFAULT_LIMIT, false),
            r"\m\%(kaigi\|かいぎ\|カイギ\|会議\|ｋａｉｇｉ\|ｶｲｷﾞ\)"
        );
    }

    /// 特別な意味を持つ文字は方言ごとに逃がす。
    #[test]
    fn special_characters_are_escaped_per_flavour() {
        // `-` は rg では特別、vim では特別でない
        assert_eq!(compress(&["-", "…"], &RG), r"[\-…]");
        assert_eq!(compress(&["-", "…"], &VIM), "[-…]");
        // `.` はどちらでも逃がす
        assert_eq!(compress(&["Ｎ.Ｙ."], &RG), r"Ｎ\.Ｙ\.");
        assert_eq!(compress(&["Ｎ.Ｙ."], &VIM), r"Ｎ\.Ｙ\.");
    }

    /// 読みは打った綴りから作る。大小は保たない。
    #[test]
    fn a_reading_comes_from_the_spelling() {
        assert_eq!(readings("kaigi", false), vec!["かいぎ"]);
        assert_eq!(readings("Kaigi", false), vec!["かいぎ"]);
        // nn は ん (自前で書き直さない)
        assert_eq!(readings("shinnyuu", false), vec!["しんゆう"]);
        // かなにならない部分は字面で残る
        assert_eq!(readings("2026", false), vec!["2026"]);
    }

    /// 半端な綴りは、続きうるかなに広げる。
    #[test]
    fn a_half_finished_spelling_expands() {
        assert_eq!(
            readings("kaig", false),
            vec!["かいが", "かいぎ", "かいぐ", "かいげ", "かいご", "かいっ"]
        );
        // 促音を挟んだ半端な綴りも同じ道を通る
        assert_eq!(readings("ttyskk", false)[0], "っtysっか");
    }

    /// 空白は語の区切りで、群が並ぶ。
    #[test]
    fn a_space_separates_words() {
        let d = dict(&[("かいぎ", "/会議/"), ("ろく", "/録/")]);
        // 一字で終わる候補 (録) はクラスの側に回るので先に出る
        assert_eq!(
            build("kaigi roku", &RG, &d, DEFAULT_LIMIT, false),
            "(?:kaigi|かいぎ|カイギ|会議|ｋａｉｇｉ|ｶｲｷﾞ)(?:録|roku|ろく|ロク|ｒｏｋｕ|ﾛｸ)"
        );
    }

    /// 読みぴったりだけでなく、その読みで始まる見出し語も引く。
    #[test]
    fn headwords_starting_with_the_reading_are_included() {
        let d = dict(&[
            ("かいぎ", "/会議/"),
            ("かいぎょう", "/開業/改行/"),
            ("かいご", "/介護/"),
        ]);
        let rx = build("kaigi", &RG, &d, DEFAULT_LIMIT, false);
        for want in ["会議", "開業", "改行"] {
            assert!(rx.contains(want), "{want} が {rx} に無い");
        }
        assert!(!rx.contains("介護"), "読みが違うものは入らない");
    }

    /// 数値変換の見出しは、そのままでは文字列として当たらないので外す。
    #[test]
    fn numeric_entries_are_left_out() {
        let d = dict(&[("だい#", "/第#0/"), ("だい", "/台/")]);
        let rx = build("dai", &RG, &d, DEFAULT_LIMIT, false);
        assert!(rx.contains('台'));
        assert!(!rx.contains('#'), "{rx} に # が残っている");
    }

    /// 上限を超えたら見出し語を切る。かなや全角英数は上限に関わらず残す。
    #[test]
    fn the_limit_cuts_only_the_headwords() {
        let many: String = (0..50).map(|i| format!("/語{i}/")).collect();
        let d = dict(&[("あ", &many)]);
        assert_eq!(headwords("a", &["あ".to_string()], &d, 3).len(), 3);

        let rx = build("a", &RG, &d, 3, false);
        assert!(rx.contains("語[012]"), "{rx}");
        for want in ["a", "あ", "ア", "ｱ", "ａ"] {
            assert!(rx.contains(want), "{want} が {rx} に無い");
        }
    }

    /// 索引を通しても、辞書をそのまま読んだ時と同じものが出る。
    #[test]
    fn the_index_gives_the_same_answer() {
        let entries: &[(&str, &str)] = &[
            ("かいぎ", "/会議/回議/"),
            ("かいぎょう", "/開業/改行/"),
            ("かいご", "/介護/"),
            ("だい#", "/第#0/"),
            ("a", "/エー/エイ/"),
        ];
        let (d, sys, dir) = dict_at(entries);
        let sources = [sys];
        let path = dir.join("migemo.index");
        Index::save(&path, &Index::build(&d, &sources)).unwrap();

        let user = Dict::load(&[], dir.join("user.dict"), None).unwrap();
        let ix = Index::load(&path, &sources, user)
            .unwrap()
            .expect("索引を読めた");

        for q in ["kaigi", "kaig", "a", "dai", "nihongo"] {
            assert_eq!(
                build(q, &RG, &ix, DEFAULT_LIMIT, false),
                build(q, &RG, &d, DEFAULT_LIMIT, false),
                "{q} で食い違う"
            );
        }
    }

    /// 元の辞書が入れ替わったら、その索引は使わない。
    #[test]
    fn a_stale_index_is_not_used() {
        let (d, sys, dir) = dict_at(&[("かいぎ", "/会議/")]);
        let sources = [sys];
        let path = dir.join("migemo.index");
        Index::save(&path, &Index::build(&d, &sources)).unwrap();

        let user = || Dict::load(&[], dir.join("user.dict"), None).unwrap();
        assert!(
            Index::load(&path, &sources, user()).unwrap().is_some(),
            "作った直後は使える"
        );

        // 元の辞書を書き換える (大きさが変わる)
        std::fs::write(&sources[0], ";; okuri-nasi entries.\nかいぎ /会議/回議/\n").unwrap();
        assert!(
            Index::load(&path, &sources, user()).unwrap().is_none(),
            "入れ替わった辞書の索引は使わない"
        );

        // 無い索引も誤りではない
        assert!(
            Index::load(&dir.join("no-such.index"), &sources, user())
                .unwrap()
                .is_none()
        );
    }

    /// 辞書に無い語でも、字面とかなの分は必ず出る。
    #[test]
    fn an_unknown_word_still_yields_the_kana() {
        let d = dict(&[]);
        assert_eq!(
            build("nihongo", &RG, &d, DEFAULT_LIMIT, false),
            "(?:nihongo|にほんご|ニホンゴ|ｎｉｈｏｎｇｏ|ﾆﾎﾝｺﾞ)"
        );
    }
}
