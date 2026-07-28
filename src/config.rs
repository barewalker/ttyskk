//! 設定ファイル。キーの割り当てを利用者が変えられるようにする。
//!
//! 既定は `~/.config/ttyskk/config.toml` (`XDG_CONFIG_HOME` を尊重、`TTYSKK_CONFIG`
//! で上書き可)。書き換えは動いている ttyskk にそのまま反映される — 更新時刻を
//! 見張る小さなスレッドが読み直し、読めた場合だけ差し替える。

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};

use crate::romaji::Kutouten;
use crate::skk::Key;
use unicode_width::UnicodeWidthChar;

/// 設定ファイルを見張る間隔。
const POLL: Duration = Duration::from_secs(1);

/// 名前で書けるエスケープ列と、その**ありうる形すべて**。
///
/// 矢印や Home / End は端末とその時の設定 (`DECCKM`) で送られる形が変わる。届いた
/// バイト列をそのまま照合する作りなので、形を一つに決め打つとその端末でだけ効かない。
/// 一つの名前に全部の形を割り当て、どれで届いても同じ働きになるようにしている。
///
/// 修飾キーの付かない矢印は拡張鍵盤プロトコルでも素の形のまま届くので、`Key::Raw` の
/// バイト列で持つ。名前を足したら [`tests::named_sequences_are_escape_sequences`] の
/// 見張りに合わせて `parse_key` の説明にも書き足す。
const NAMED_SEQUENCES: &[(&str, &[&[u8]])] = &[
    // CSI 形 (既定) と SS3 形 (アプリケーションカーソルキーモード)
    ("left", &[b"\x1b[D", b"\x1bOD"]),
    ("right", &[b"\x1b[C", b"\x1bOC"]),
    ("up", &[b"\x1b[A", b"\x1bOA"]),
    ("down", &[b"\x1b[B", b"\x1bOB"]),
    // 末尾の二つは linux コンソールと rxvt の流儀
    ("home", &[b"\x1b[H", b"\x1bOH", b"\x1b[1~", b"\x1b[7~"]),
    ("end", &[b"\x1b[F", b"\x1bOF", b"\x1b[4~", b"\x1b[8~"]),
];

/// 名前の付いたエスケープ列を、ありうる形すべての [`Key`] にする。
fn named_sequence(name: &str) -> Option<Vec<Key>> {
    NAMED_SEQUENCES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, seqs)| seqs.iter().map(|s| Key::Raw(s.to_vec())).collect())
}

/// 制御キー一つと、名前の付いたエスケープ列を並べた割り当て。既定を組むのに使う。
fn ctrl_or_sequence(ctrl: u8, name: &str) -> Vec<Key> {
    let mut out = vec![Key::Ctrl(ctrl)];
    out.extend(named_sequence(name).expect("NAMED_SEQUENCES に無い名前"));
    out
}

/// 設定の見本。実行ファイルに埋め込んで `--config-example` で書き出す。
///
/// 別に配らなくてよいうえ、版とずれない。中身が既定と食い違っていないことは
/// [`tests::the_bundled_example_states_the_defaults`] が見張る。
pub const EXAMPLE: &str = include_str!("../config.example.toml");

/// モードの印の出し方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// 何も描かない。カーソルの形だけでモードを表す。
    Off,
    /// カーソル位置のセルにモードの色を敷く。文字を足さない。
    ///
    /// カーソルの形がブロックだと色を覆ってしまうので、この方式のときは
    /// 形を下線に固定する。形 = 動いている合図、色 = モード、という配分。
    /// 素の端末ではカーソルそのものが色付いて見えて具合がよい。
    Cell,
    /// カーソル位置のセルにモードを表す半角の記号を出す。
    ///
    /// 色だけに頼らないので、モノクロの環境でも区別が付く。幅は 1 桁なので
    /// 見た目も崩れない。ただし下にある文字は隠れる (`Cell` は隠さない)。
    /// 記号は `mode_symbols` で変えられる。
    Symbol,
    /// カーソルの**右隣**のセルに色を敷く。文字を足さない。
    ///
    /// カーソルに覆われないので、端末多重化器がカーソルの見た目を遅れて
    /// 同期する環境 (herdr がそう) でも確実に見える。
    Beside,
    /// カーソルの直後に あ / ア / 半 / Ａ を出す。
    Letter,
}

/// 候補一覧の出し方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// 入力中の行に続けて横並び。行が伸びるぶん折り返すことがある。
    Inline,
    /// カーソルの下の行 (最下行なら上の行) に一行で浮かせる。
    Float,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// ASCII / 全角英数から かなモードへ入る
    pub kana: Vec<Key>,
    /// 入力中のローマ字・見出し語・候補を確定する
    pub confirm: Vec<Key>,
    /// 取り消す
    pub cancel: Vec<Key>,
    /// ASCII モードへ
    pub ascii: Vec<Key>,
    /// 全角英数モードへ
    pub zenkaku: Vec<Key>,
    /// ひらがな ⇄ カタカナ。▽ の途中では見出し語をカタカナにして確定する
    pub katakana: Vec<Key>,
    /// ひらがな ⇄ 半角カタカナ。▽ の途中では見出し語を半角カタカナにして確定する
    pub hankaku_katakana: Vec<Key>,
    /// 空の見出し語で変換を始める (複合語向け)
    pub start_conversion: Vec<Key>,
    /// ASCII の見出し語で変換する
    pub abbrev: Vec<Key>,
    /// 変換する / 次の候補へ
    pub convert: Vec<Key>,
    /// 前の候補へ
    pub previous: Vec<Key>,
    /// ▽ の見出し語を前方一致で補完する / 次の補完候補へ
    pub complete: Vec<Key>,
    /// 前の補完候補へ
    pub complete_previous: Vec<Key>,
    /// ▼ の候補を利用者辞書から取り除く
    pub purge: Vec<Key>,
    /// 接頭辞・接尾辞変換を始める (▽ の末尾に付けるか、▼ を確定して `>` から始める)
    pub affix: Vec<Key>,
    /// ▽ の見出し語の中でカーソルを一文字左へ
    pub move_left: Vec<Key>,
    /// ▽ の見出し語の中でカーソルを一文字右へ
    pub move_right: Vec<Key>,
    /// ▽ の見出し語の先頭へ
    pub move_home: Vec<Key>,
    /// ▽ の見出し語の末尾へ
    pub move_end: Vec<Key>,
    /// ▽ のカーソル位置にある一文字を消す
    pub delete_forward: Vec<Key>,
    /// 候補一覧から選ぶキー。並び順がそのまま一覧の並び順になる
    pub select: Vec<char>,
    /// 一覧を出さずに一つずつ送る候補数
    pub inline_candidates: usize,
    /// 候補一覧の出し方
    pub layout: Layout,
    /// `mode_marker = "symbol"` で出す記号。
    ///
    /// 順に ひらがな / カタカナ / 半角カタカナ / 全角英数。既定は `~ + - @` で、
    /// ひらがなは曲線的、カタカナは角ばった形、半角カタカナは `+` から縦棒を
    /// 取った形 (半分)、全角英数は英数の `@`、という覚え方にしてある。
    /// 英字は本文と紛れるので記号にしてある。
    pub mode_symbols: [char; 4],
    /// モードの印の出し方。
    ///
    /// 端末多重化器を挟むとカーソルの色 (OSC 12) が途中で吸われる (herdr が実際に
    /// そう)。文字として書いた色はそのまま届くので、色でモードを見分けたい場合は
    /// これを使う。ASCII モードでは何も描かないので、そのときの完全透過は保つ。
    pub mode_marker: Marker,
    /// `.` と `,` から出す句読点の組。
    pub kutouten: Kutouten,
    /// AZIK (拡張ローマ字入力) を使うか。
    ///
    /// 「2 文字めに《ん》が来る」「二重母音」の並びを 2 打で打てるようにする方式。
    /// 標準のローマ字を土台にしているので、打てなくなる綴りは無い。
    pub azik: bool,
    /// 見出し語の入力中にこれらの文字が来たら、その手前までで変換を始める。
    ///
    /// 「ほんやくを」と打つと `を` の直前で変換に入り、`を` は候補の後ろに置かれる。
    /// ddskk の `skk-auto-start-henkan-keyword-list` と同じ。空にすると自動変換を
    /// しない。
    pub auto_start_henkan: Vec<char>,
    /// 接頭辞・接尾辞に続けて確定した語を、繋げて辞書に覚えるか。
    ///
    /// 「さい>」→再 のあと「りよう」→利用 と確定したら `さいりよう /再利用/` を
    /// 覚える。ddskk の `skk-learn-combined-word` と同じ (既定は有効)。
    pub learn_combined: bool,
    /// 押したときに ASCII モードへ戻すキー。空なら何もしない。
    ///
    /// vim / nvim で挿入モードを抜けたときに、かなモードが残らないようにする。
    /// 既定は `Esc` と `C-c` — nvim で実測したところ、この二つは挿入モードを
    /// 抜けるが `C-d` は抜けない (インデントを一段戻す)。
    pub ascii_keys: Vec<Key>,
    /// スニペット (定型文) を書いたファイル。
    ///
    /// 住所や電話番号、メールの署名のような「打つのが面倒で、内容が決まっている」
    /// 文字列を、変換の候補として引けるようにする。書式は VS Code の
    /// `*.code-snippets` ([`crate::snippet`])。
    ///
    /// **空なら既定の置き場所を一つだけ読む** (`~/.local/share/ttyskk/` の
    /// `snippets.code-snippets`)。書けば、そこに挙げたものだけを読む。学習と
    /// 混ぜたくない場合や、他の人と分け合いたい場合に置き場所を移せる。
    pub snippets: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            kana: vec![Key::Ctrl(0x0a)],
            confirm: vec![Key::Ctrl(0x0a)],
            cancel: vec![Key::Ctrl(0x07)],
            ascii: vec![Key::Char('l')],
            zenkaku: vec![Key::Char('L')],
            katakana: vec![Key::Char('q')],
            hankaku_katakana: vec![Key::Ctrl(0x11)],
            start_conversion: vec![Key::Char('Q')],
            abbrev: vec![Key::Char('/')],
            convert: vec![Key::Char(' ')],
            previous: vec![Key::Char('x')],
            complete: vec![Key::Tab],
            complete_previous: vec![Key::ShiftTab],
            purge: vec![Key::Char('X')],
            affix: vec![Key::Char('>')],
            // C-b / C-f / C-a / C-e は emacs と読み方の同じ移動。矢印でも動くように
            // しておくと、行編集の流儀を持ち込まない人もそのまま使える。
            move_left: ctrl_or_sequence(0x02, "left"),
            move_right: ctrl_or_sequence(0x06, "right"),
            move_home: ctrl_or_sequence(0x01, "home"),
            move_end: ctrl_or_sequence(0x05, "end"),
            delete_forward: vec![Key::Ctrl(0x04)],
            select: vec!['a', 's', 'd', 'f', 'j', 'k', 'l'],
            inline_candidates: 4,
            layout: Layout::Inline,
            mode_symbols: ['~', '+', '-', '@'],
            mode_marker: Marker::Cell,
            kutouten: Kutouten::Jp,
            azik: false,
            // ddskk の skk-auto-start-henkan-keyword-list と同じ顔ぶれ
            auto_start_henkan: "を、。．，？」！；：);:）”】』》〉}]?.,!".chars().collect(),
            learn_combined: true,
            ascii_keys: vec![Key::Esc, Key::Ctrl(0x03)],
            snippets: Vec::new(),
        }
    }
}

impl Config {
    /// 一覧一頁あたりの候補数。選択キーの数がそのまま頁の大きさになる。
    pub fn page_size(&self) -> usize {
        self.select.len().max(1)
    }

    /// SKK 自身の操作に割り当てられているキーを節ごとに並べて返す。
    ///
    /// **キーの項目を足したらここにも足す。** 拡張鍵盤プロトコルの復号
    /// (`input::Decoder`) がこの並びを唯一の頼りにしているので、書き漏らすと
    /// そのキーだけ Claude Code のようなアプリの下で効かなくなる。
    ///
    /// `ascii_keys` は**入れない**。あれは子アプリが自分の操作に使っているキー
    /// (vim の `Esc` / `C-c`) に便乗して ASCII へ戻すためのもので、押されたキーは
    /// そのまま子へ渡る。持ち主でないキーの形を変えると子の操作が変質する。
    fn key_slots(&self) -> [&Vec<Key>; 20] {
        [
            &self.kana,
            &self.confirm,
            &self.cancel,
            &self.ascii,
            &self.zenkaku,
            &self.katakana,
            &self.hankaku_katakana,
            &self.start_conversion,
            &self.abbrev,
            &self.convert,
            &self.previous,
            &self.complete,
            &self.complete_previous,
            &self.purge,
            &self.affix,
            &self.move_left,
            &self.move_right,
            &self.move_home,
            &self.move_end,
            &self.delete_forward,
        ]
    }

    /// いま SKK 自身の操作に割り当てられているキー。
    ///
    /// 拡張鍵盤プロトコルの下では修飾キーの付いた打鍵が `CSI 106;5u` のような形で
    /// 届く。素の形に戻すのはここに挙がったキーだけにして、割り当ての無いキーは
    /// 元のバイト列のまま子へ渡す (`input::Decoder`)。
    pub fn bound_keys(&self) -> Vec<Key> {
        let mut out = Vec::new();
        for slot in self.key_slots() {
            for k in slot {
                if !out.contains(k) {
                    out.push(k.clone());
                }
            }
        }
        out
    }

    /// 設定ファイルを読む。無ければ既定を返す。
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => bail!("{} を読めない: {e}", path.display()),
        }
    }

    pub fn parse(text: &str) -> Result<Config> {
        let table: toml::Table = text.parse().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut cfg = Config::default();

        if let Some(keys) = table.get("keys") {
            let keys = keys
                .as_table()
                .ok_or_else(|| anyhow::anyhow!("[keys] は表でなければならない"))?;
            for (name, value) in keys {
                let slot = match name.as_str() {
                    "kana" => &mut cfg.kana,
                    "confirm" => &mut cfg.confirm,
                    "cancel" => &mut cfg.cancel,
                    "ascii" => &mut cfg.ascii,
                    "zenkaku" => &mut cfg.zenkaku,
                    "katakana" => &mut cfg.katakana,
                    "hankaku_katakana" => &mut cfg.hankaku_katakana,
                    "start_conversion" => &mut cfg.start_conversion,
                    "abbrev" => &mut cfg.abbrev,
                    "convert" => &mut cfg.convert,
                    "previous" => &mut cfg.previous,
                    "complete" => &mut cfg.complete,
                    "complete_previous" => &mut cfg.complete_previous,
                    "purge" => &mut cfg.purge,
                    "affix" => &mut cfg.affix,
                    "move_left" => &mut cfg.move_left,
                    "move_right" => &mut cfg.move_right,
                    "move_home" => &mut cfg.move_home,
                    "move_end" => &mut cfg.move_end,
                    "delete_forward" => &mut cfg.delete_forward,
                    "select" => {
                        cfg.select = parse_select(value)?;
                        continue;
                    }
                    other => bail!("keys.{other} は知らない項目"),
                };
                *slot = parse_keys(name, value)?;
            }
        }

        if let Some(b) = table.get("behavior") {
            let b = b
                .as_table()
                .ok_or_else(|| anyhow::anyhow!("[behavior] は表でなければならない"))?;
            for (name, value) in b {
                match name.as_str() {
                    // ここだけは空の並びを許す (割り当てを外す指定になる)
                    "ascii_keys" => {
                        cfg.ascii_keys = match value {
                            toml::Value::Array(a) if a.is_empty() => Vec::new(),
                            v => parse_keys("ascii_keys", v)?,
                        }
                    }
                    "mode_marker" => {
                        cfg.mode_marker = match value.as_str() {
                            Some("off") => Marker::Off,
                            Some("cell") => Marker::Cell,
                            Some("symbol") => Marker::Symbol,
                            Some("beside") => Marker::Beside,
                            Some("letter") => Marker::Letter,
                            _ => bail!(
                                "behavior.mode_marker は \"off\" / \"cell\" / \"symbol\" / \"beside\" / \"letter\""
                            ),
                        }
                    }
                    "mode_symbols" => {
                        let t = value.as_table().ok_or_else(|| {
                            anyhow::anyhow!("behavior.mode_symbols は表 (名前 = 記号)")
                        })?;
                        for (name, v) in t {
                            let i = match name.as_str() {
                                "hiragana" => 0,
                                "katakana" => 1,
                                "hankaku_katakana" => 2,
                                "zenkaku" => 3,
                                other => bail!("mode_symbols.{other} は知らない項目"),
                            };
                            cfg.mode_symbols[i] = parse_symbol(name, v)?;
                        }
                    }
                    "romaji" => {
                        cfg.azik = match value.as_str() {
                            Some("default") => false,
                            Some("azik") => true,
                            _ => bail!("behavior.romaji は \"default\" か \"azik\""),
                        }
                    }
                    "kutouten" => {
                        cfg.kutouten = match value.as_str() {
                            Some("jp") => Kutouten::Jp,
                            Some("en") => Kutouten::En,
                            Some("jp-en") => Kutouten::JpEn,
                            Some("en-jp") => Kutouten::EnJp,
                            _ => bail!(
                                "behavior.kutouten は \"jp\" (。、) / \"en\" (．，) / \"jp-en\" (。，) / \"en-jp\" (．、)"
                            ),
                        }
                    }
                    // ここも空の並びを許す (自動変換をやめる指定になる)
                    "auto_start_henkan" => {
                        cfg.auto_start_henkan = parse_chars("auto_start_henkan", value)?
                    }
                    "learn_combined" => {
                        cfg.learn_combined = value
                            .as_bool()
                            .ok_or_else(|| anyhow::anyhow!("behavior.learn_combined は真偽値"))?
                    }
                    other => bail!("behavior.{other} は知らない項目"),
                }
            }
        }

        if let Some(c) = table.get("candidates") {
            let c = c
                .as_table()
                .ok_or_else(|| anyhow::anyhow!("[candidates] は表でなければならない"))?;
            for (name, value) in c {
                match name.as_str() {
                    "inline" => {
                        let n = value
                            .as_integer()
                            .ok_or_else(|| anyhow::anyhow!("candidates.inline は整数"))?;
                        if n < 1 {
                            bail!("candidates.inline は 1 以上");
                        }
                        cfg.inline_candidates = n as usize;
                    }
                    "layout" => {
                        cfg.layout = match value.as_str() {
                            Some("inline") => Layout::Inline,
                            Some("float") => Layout::Float,
                            _ => bail!("candidates.layout は \"inline\" か \"float\""),
                        }
                    }
                    other => bail!("candidates.{other} は知らない項目"),
                }
            }
        }

        if let Some(s) = table.get("snippets") {
            let s = s
                .as_table()
                .ok_or_else(|| anyhow::anyhow!("[snippets] は表でなければならない"))?;
            for (name, value) in s {
                match name.as_str() {
                    "files" => cfg.snippets = parse_paths(name, value)?,
                    other => bail!("snippets.{other} は知らない項目"),
                }
            }
        }

        for (name, value) in &table {
            if !matches!(
                name.as_str(),
                "keys" | "candidates" | "behavior" | "snippets"
            ) {
                let _ = value;
                bail!("[{name}] は知らない節");
            }
        }
        Ok(cfg)
    }
}

/// 置き場所の並びを読む。文字列一つでも並びでもよい。
///
/// 先頭の `~/` は自分のホームに開く。設定ファイルは手で書くものなので、
/// `/home/…` を書かせずに済ませたい。
fn parse_paths(name: &str, value: &toml::Value) -> Result<Vec<PathBuf>> {
    let list = match value {
        toml::Value::String(s) => vec![s.as_str()],
        toml::Value::Array(a) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| anyhow::anyhow!("snippets.{name} の並びは文字列だけ"))
            })
            .collect::<Result<Vec<_>>>()?,
        _ => bail!("snippets.{name} は文字列か文字列の並び"),
    };
    Ok(list.into_iter().map(expand_home).collect())
}

/// 先頭の `~/` をホームに開く。開けなければそのまま返す。
fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    }
}

fn parse_keys(name: &str, value: &toml::Value) -> Result<Vec<Key>> {
    let specs: Vec<&str> = match value {
        toml::Value::String(s) => vec![s.as_str()],
        toml::Value::Array(a) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| anyhow::anyhow!("keys.{name} の並びは文字列だけ"))
            })
            .collect::<Result<_>>()?,
        _ => bail!("keys.{name} は文字列か文字列の並び"),
    };
    if specs.is_empty() {
        bail!("keys.{name} が空。割り当てを外したいなら項目ごと消す");
    }
    let mut out = Vec::new();
    for spec in specs {
        out.extend(parse_key(spec)?);
    }
    Ok(out)
}

fn parse_select(value: &toml::Value) -> Result<Vec<char>> {
    let arr = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("keys.select は文字の並び"))?;
    let mut out = Vec::new();
    for v in arr {
        let s = v
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("keys.select の並びは文字列だけ"))?;
        let mut it = s.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => out.push(c),
            _ => bail!("keys.select は一文字ずつ書く ({s} は不可)"),
        }
    }
    if out.is_empty() {
        bail!("keys.select が空");
    }
    Ok(out)
}

/// 文字の集まり。まとめた文字列一つでも、一文字ずつの並びでも受ける。
///
/// 空の並びは「その働きをやめる」の意味になるので許す。
fn parse_chars(name: &str, value: &toml::Value) -> Result<Vec<char>> {
    match value {
        toml::Value::String(s) => Ok(s.chars().collect()),
        toml::Value::Array(a) => {
            let mut out = Vec::new();
            for v in a {
                let s = v
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("behavior.{name} の並びは文字列だけ"))?;
                out.extend(s.chars());
            }
            Ok(out)
        }
        _ => bail!("behavior.{name} は文字列か文字列の並び"),
    }
}

/// モードの印に使う記号。半角の一文字だけを認める。
///
/// 全角を許すと幅が二桁になり、「見た目が崩れない」という約束が壊れる。
fn parse_symbol(name: &str, value: &toml::Value) -> Result<char> {
    let s = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("mode_symbols.{name} は文字列"))?;
    let mut it = s.chars();
    let (Some(c), None) = (it.next(), it.next()) else {
        bail!("mode_symbols.{name} は一文字だけ ({s} は不可)");
    };
    if c.width().unwrap_or(0) != 1 {
        bail!("mode_symbols.{name} は半角一桁の文字だけ ({c} は不可)");
    }
    Ok(c)
}

/// キーの書き方を `Key` にする。
///
/// `C-j` `Ctrl-j` `ctrl+j` / `space` `enter` `tab` `esc` `bs` /
/// `left` `right` `up` `down` `home` `end` / 一文字そのもの。
///
/// **返るのは一つとは限らない。** 矢印のように端末で送られる形が変わるキーは、
/// ありうる形をすべて返す ([`NAMED_SEQUENCES`])。
fn parse_key(spec: &str) -> Result<Vec<Key>> {
    let s = spec.trim();
    if s.is_empty() {
        bail!("キーの指定が空");
    }
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "space" | "spc" => return Ok(vec![Key::Char(' ')]),
        "enter" | "return" | "ret" => return Ok(vec![Key::Enter]),
        "tab" => return Ok(vec![Key::Tab]),
        "esc" | "escape" => return Ok(vec![Key::Esc]),
        "bs" | "backspace" | "del" => return Ok(vec![Key::Backspace]),
        "s-tab" | "shift-tab" | "btab" => return Ok(vec![Key::ShiftTab]),
        _ => {}
    }
    if let Some(keys) = named_sequence(&lower) {
        return Ok(keys);
    }
    for prefix in ["c-", "ctrl-", "ctrl+", "control-"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            // 端末では Ctrl+Space は NUL として届く
            if matches!(rest, "space" | "spc") {
                return Ok(vec![Key::Ctrl(0x00)]);
            }
            let mut it = rest.chars();
            let (Some(c), None) = (it.next(), it.next()) else {
                bail!("{spec} は解せない (Ctrl は一文字にだけ付く)");
            };
            if !c.is_ascii_alphabetic() {
                bail!("{spec} は解せない (Ctrl は英字にだけ付く)");
            }
            // C-a = 0x01 … C-z = 0x1a
            return Ok(vec![Key::Ctrl(c as u8 & 0x1f)]);
        }
    }
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Ok(vec![Key::Char(c)]),
        _ => bail!("{spec} は解せない"),
    }
}

/// 設定ファイルの場所。
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("TTYSKK_CONFIG") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ttyskk/config.toml")
}

/// 設定ファイルの更新を見張り、読み直せたら送る。
///
/// 更新時刻だけを見る。編集器が別名で書いて置き換える場合も、消してから作り直す
/// 場合も同じように拾える。読めない設定は捨てて、それまでの設定を使い続ける。
pub fn watch<F>(path: PathBuf, mut on_change: F)
where
    F: FnMut(Config) + Send + 'static,
{
    let p = path.clone();
    watch_path(path, move || {
        if let Ok(cfg) = Config::load(&p) {
            on_change(cfg);
        }
    });
}

/// ファイルの書き換えを見張り、変わるたびに知らせる。
///
/// 更新時刻と大きさだけを見るので、中身が何であっても使える。利用者辞書のように、
/// **別のプロセスが書き換えるもの**を追うのにも使う。
pub fn watch_path<F>(path: PathBuf, mut on_change: F)
where
    F: FnMut() + Send + 'static,
{
    std::thread::spawn(move || {
        let mut last = stamp(&path);
        loop {
            std::thread::sleep(POLL);
            let now = stamp(&path);
            if now == last {
                continue;
            }
            last = now;
            on_change();
        }
    });
}

/// 存在しない場合も含めた「いまの状態」。
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// `watch` の結果を送り先へ流すための小さな橋渡し。
pub fn watch_into<T: Send + 'static>(path: PathBuf, tx: Sender<T>, wrap: fn(Config) -> T) {
    watch(path, move |cfg| {
        let _ = tx.send(wrap(cfg));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_bindings() {
        let c = Config::default();
        assert_eq!(c.kana, vec![Key::Ctrl(0x0a)]);
        assert_eq!(c.cancel, vec![Key::Ctrl(0x07)]);
        assert_eq!(c.convert, vec![Key::Char(' ')]);
        assert_eq!(c.page_size(), 7);
    }

    #[test]
    fn bound_keys_lists_what_skk_uses() {
        let c = Config::default().bound_keys();
        assert!(c.contains(&Key::Ctrl(0x0a)), "C-j"); // kana / confirm
        assert!(c.contains(&Key::Ctrl(0x07)), "C-g"); // cancel
        assert!(c.contains(&Key::Ctrl(0x11)), "C-q"); // hankaku_katakana
        assert!(c.contains(&Key::ShiftTab), "S-tab"); // complete_previous
        // 割り当ての無いキーは挙げない (子アプリの操作を奪わない)
        assert!(!c.contains(&Key::Ctrl(0x1a)), "C-z");
        // ascii_keys は子アプリのキーへの便乗なので、形を変えずに渡す
        assert!(!c.contains(&Key::Ctrl(0x03)), "C-c");
    }

    /// キーの項目を足して `key_slots` に足し忘れると、そのキーだけ拡張鍵盤
    /// プロトコルを使うアプリの下で効かなくなる。全項目を Ctrl に割り当てて見張る。
    #[test]
    fn bound_keys_covers_every_binding() {
        let names = [
            "kana",
            "confirm",
            "cancel",
            "ascii",
            "zenkaku",
            "katakana",
            "hankaku_katakana",
            "start_conversion",
            "abbrev",
            "convert",
            "previous",
            "complete",
            "complete_previous",
            "purge",
            "affix",
            "move_left",
            "move_right",
            "move_home",
            "move_end",
            "delete_forward",
        ];
        let letters = "abcdefghijklmnoqrstu";
        assert_eq!(names.len(), letters.len());
        let mut text = String::from("[keys]\n");
        for (name, c) in names.iter().zip(letters.chars()) {
            text.push_str(&format!("{name} = \"C-{c}\"\n"));
        }
        text.push_str("\n[behavior]\nascii_keys = [\"C-p\"]\n");

        let got = Config::parse(&text).unwrap().bound_keys();
        for c in letters.chars() {
            assert!(
                got.contains(&Key::Ctrl(c as u8 & 0x1f)),
                "C-{c} が漏れている"
            );
        }
        assert!(
            !got.contains(&Key::Ctrl(0x10)),
            "ascii_keys の C-p は対象外"
        );
    }

    #[test]
    fn parses_key_spellings() {
        assert_eq!(parse_key("C-j").unwrap(), vec![Key::Ctrl(0x0a)]);
        assert_eq!(parse_key("ctrl+g").unwrap(), vec![Key::Ctrl(0x07)]);
        assert_eq!(parse_key("Ctrl-A").unwrap(), vec![Key::Ctrl(0x01)]);
        assert_eq!(parse_key("space").unwrap(), vec![Key::Char(' ')]);
        assert_eq!(parse_key("Enter").unwrap(), vec![Key::Enter]);
        assert_eq!(parse_key("esc").unwrap(), vec![Key::Esc]);
        assert_eq!(parse_key("bs").unwrap(), vec![Key::Backspace]);
        assert_eq!(parse_key("C-space").unwrap(), vec![Key::Ctrl(0x00)]);
        assert_eq!(parse_key("/").unwrap(), vec![Key::Char('/')]);
        assert_eq!(parse_key("Q").unwrap(), vec![Key::Char('Q')]);
        assert!(parse_key("C-").is_err());
        assert!(parse_key("C-1").is_err());
        assert!(parse_key("").is_err());
    }

    /// 矢印は端末で形が変わるので、一つの名前がありうる形すべてに広がる。
    #[test]
    fn arrow_names_cover_every_shape_the_terminal_may_send() {
        assert_eq!(
            parse_key("left").unwrap(),
            vec![Key::Raw(b"\x1b[D".to_vec()), Key::Raw(b"\x1bOD".to_vec()),],
            "CSI 形と SS3 形 (アプリケーションカーソルキーモード) の両方"
        );
        assert_eq!(parse_key("Right").unwrap().len(), 2, "大文字でも引ける");
        assert_eq!(parse_key("home").unwrap().len(), 4);

        // 設定に書けば、その名前の全部の形が割り当てになる
        let c = Config::parse("[keys]\nmove_left = [\"C-b\", \"left\"]\n").unwrap();
        assert_eq!(
            c.move_left,
            vec![
                Key::Ctrl(0x02),
                Key::Raw(b"\x1b[D".to_vec()),
                Key::Raw(b"\x1bOD".to_vec()),
            ]
        );
    }

    /// 名前の付いたエスケープ列は、本当にエスケープ列でなければならない。
    ///
    /// ここに素の文字を書くと、その文字が打てなくなる。
    #[test]
    fn named_sequences_are_escape_sequences() {
        for (name, seqs) in NAMED_SEQUENCES {
            assert!(!seqs.is_empty(), "{name} に形が無い");
            for s in *seqs {
                assert_eq!(
                    s.first(),
                    Some(&0x1b),
                    "{name} の {s:?} が ESC で始まらない"
                );
            }
        }
    }

    /// 既定の移動キーは C-b / C-f / C-a / C-e と矢印の両方。
    #[test]
    fn cursor_keys_default_to_both_control_and_arrows() {
        let c = Config::default();
        for (slot, ctrl, seq) in [
            (&c.move_left, 0x02, "\x1b[D"),
            (&c.move_right, 0x06, "\x1b[C"),
            (&c.move_home, 0x01, "\x1b[H"),
            (&c.move_end, 0x05, "\x1b[F"),
        ] {
            assert!(slot.contains(&Key::Ctrl(ctrl)), "C-{ctrl:#x} が無い");
            assert!(
                slot.contains(&Key::Raw(seq.as_bytes().to_vec())),
                "{seq:?} が無い"
            );
        }
        assert_eq!(c.delete_forward, vec![Key::Ctrl(0x04)]);
    }

    #[test]
    fn empty_file_gives_the_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn overrides_only_what_is_written() {
        let c = Config::parse(
            r#"
            [keys]
            kana = "C-o"
            cancel = ["C-g", "esc"]
            select = ["1", "2", "3"]

            [candidates]
            inline = 2
            "#,
        )
        .unwrap();
        assert_eq!(c.kana, vec![Key::Ctrl(0x0f)]);
        assert_eq!(c.cancel, vec![Key::Ctrl(0x07), Key::Esc]);
        assert_eq!(c.select, vec!['1', '2', '3']);
        assert_eq!(c.page_size(), 3);
        assert_eq!(c.inline_candidates, 2);
        // behavior は空の並びだけ「無効」の意味で許す
        assert_eq!(
            Config::parse("[behavior]\nascii_keys = []\n")
                .unwrap()
                .ascii_keys,
            Vec::<Key>::new()
        );
        assert_eq!(
            Config::parse("[behavior]\nascii_keys = [\"esc\", \"C-c\"]\n")
                .unwrap()
                .ascii_keys,
            vec![Key::Esc, Key::Ctrl(0x03)]
        );
        // 書いていない項目は既定のまま
        assert_eq!(c.ascii, vec![Key::Char('l')]);
    }

    #[test]
    fn rejects_what_it_cannot_honour() {
        assert!(Config::parse("[keys]\nkana = 3\n").is_err());
        assert!(Config::parse("[keys]\nkanaa = \"x\"\n").is_err());
        assert!(Config::parse("[keyz]\n").is_err());
        assert!(Config::parse("[keys]\nselect = [\"ab\"]\n").is_err());
        assert!(Config::parse("[candidates]\ninline = 0\n").is_err());
        assert!(Config::parse("[keys]\ncancel = []\n").is_err());
        assert!(Config::parse("[behavior]\nascii_keys = 3\n").is_err());
        assert!(Config::parse("[behavior]\nfoo = []\n").is_err());
    }

    /// 同梱の見本が、書いてあるとおりに動くこと。
    ///
    /// 見本は全項目を既定値のまま `#` で無効にしてある。そのまま読めば既定、`#` を
    /// 外しても既定 — この二つが揃って初めて「既定値を並べた見本」が正しい。既定を
    /// 変えたのに見本を直し忘れると、後者が食い違って落ちる。
    #[test]
    fn the_bundled_example_states_the_defaults() {
        let text = EXAMPLE;
        assert_eq!(
            Config::parse(text).unwrap(),
            Config::default(),
            "見本をそのまま読んだら既定になるはず"
        );

        // 説明の行は「# 」(空白付き)、無効にした設定の行は「#」の直後から始まる。
        let enabled: String = text
            .lines()
            .map(|line| match line.strip_prefix('#') {
                Some(rest) if !rest.starts_with(' ') && !rest.is_empty() => rest,
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            Config::parse(&enabled).unwrap(),
            Config::default(),
            "見本の # を外したら既定と同じになるはず (既定を変えたら見本も直す)"
        );
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let p = std::env::temp_dir().join("ttyskk-no-such-config-file.toml");
        let _ = std::fs::remove_file(&p);
        assert_eq!(Config::load(&p).unwrap(), Config::default());
    }
}
