//! 設定ファイル。キーの割り当てを利用者が変えられるようにする。
//!
//! 既定は `~/.config/ttyskk/config.toml` (`XDG_CONFIG_HOME` を尊重、`TTYSKK_CONFIG`
//! で上書き可)。書き換えは動いている ttyskk にそのまま反映される — 更新時刻を
//! 見張る小さなスレッドが読み直し、読めた場合だけ差し替える。

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};

use crate::skk::Key;
use unicode_width::UnicodeWidthChar;

/// 設定ファイルを見張る間隔。
const POLL: Duration = Duration::from_secs(1);

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
            complete_previous: vec![Key::Raw(b"\x1b[Z".to_vec())],
            purge: vec![Key::Char('X')],
            affix: vec![Key::Char('>')],
            select: vec!['a', 's', 'd', 'f', 'j', 'k', 'l'],
            inline_candidates: 4,
            layout: Layout::Inline,
            mode_symbols: ['~', '+', '-', '@'],
            mode_marker: Marker::Cell,
            learn_combined: true,
            ascii_keys: vec![Key::Esc, Key::Ctrl(0x03)],
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
    fn key_slots(&self) -> [&Vec<Key>; 15] {
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
        ]
    }

    /// いま SKK 自身の操作に割り当てられている Ctrl 付きキーの制御文字。
    ///
    /// 拡張鍵盤プロトコルの下では Ctrl 付きのキーが `CSI 106;5u` のような形で
    /// 届く。素の制御文字に戻すのはここに挙がったキーだけにして、割り当ての
    /// 無いキーは元のバイト列のまま子へ渡す (`input::Decoder`)。
    pub fn ctrl_keys(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for slot in self.key_slots() {
            for k in slot {
                if let Key::Ctrl(b) = k
                    && !out.contains(b)
                {
                    out.push(*b);
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

        for (name, value) in &table {
            if !matches!(name.as_str(), "keys" | "candidates" | "behavior") {
                let _ = value;
                bail!("[{name}] は知らない節");
            }
        }
        Ok(cfg)
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
    specs.into_iter().map(parse_key).collect()
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
/// `C-j` `Ctrl-j` `ctrl+j` / `space` `enter` `tab` `esc` `bs` / 一文字そのもの。
fn parse_key(spec: &str) -> Result<Key> {
    let s = spec.trim();
    if s.is_empty() {
        bail!("キーの指定が空");
    }
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "space" | "spc" => return Ok(Key::Char(' ')),
        "enter" | "return" | "ret" => return Ok(Key::Enter),
        "tab" => return Ok(Key::Tab),
        "esc" | "escape" => return Ok(Key::Esc),
        "bs" | "backspace" | "del" => return Ok(Key::Backspace),
        // 端末は Shift+Tab を CSI Z として送る
        "s-tab" | "shift-tab" | "btab" => return Ok(Key::Raw(b"\x1b[Z".to_vec())),
        _ => {}
    }
    for prefix in ["c-", "ctrl-", "ctrl+", "control-"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            // 端末では Ctrl+Space は NUL として届く
            if matches!(rest, "space" | "spc") {
                return Ok(Key::Ctrl(0x00));
            }
            let mut it = rest.chars();
            let (Some(c), None) = (it.next(), it.next()) else {
                bail!("{spec} は解せない (Ctrl は一文字にだけ付く)");
            };
            if !c.is_ascii_alphabetic() {
                bail!("{spec} は解せない (Ctrl は英字にだけ付く)");
            }
            // C-a = 0x01 … C-z = 0x1a
            return Ok(Key::Ctrl(c as u8 & 0x1f));
        }
    }
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Ok(Key::Char(c)),
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
    std::thread::spawn(move || {
        let mut last = stamp(&path);
        loop {
            std::thread::sleep(POLL);
            let now = stamp(&path);
            if now == last {
                continue;
            }
            last = now;
            if let Ok(cfg) = Config::load(&path) {
                on_change(cfg);
            }
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
    fn ctrl_keys_lists_the_bound_control_characters() {
        let c = Config::default().ctrl_keys();
        assert!(c.contains(&0x0a), "C-j"); // kana / confirm
        assert!(c.contains(&0x07), "C-g"); // cancel
        assert!(c.contains(&0x11), "C-q"); // hankaku_katakana
        // 割り当ての無いキーは挙げない (子アプリの操作を奪わない)
        assert!(!c.contains(&0x1a), "C-z");
        // ascii_keys は子アプリのキーへの便乗なので、形を変えずに渡す
        assert!(!c.contains(&0x03), "C-c");
    }

    /// キーの項目を足して `key_slots` に足し忘れると、そのキーだけ拡張鍵盤
    /// プロトコルを使うアプリの下で効かなくなる。全項目を Ctrl に割り当てて見張る。
    #[test]
    fn ctrl_keys_covers_every_binding() {
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
        ];
        let letters = "abcdefghijklmno";
        assert_eq!(names.len(), letters.len());
        let mut text = String::from("[keys]\n");
        for (name, c) in names.iter().zip(letters.chars()) {
            text.push_str(&format!("{name} = \"C-{c}\"\n"));
        }
        text.push_str("\n[behavior]\nascii_keys = [\"C-p\"]\n");

        let got = Config::parse(&text).unwrap().ctrl_keys();
        for c in letters.chars() {
            assert!(got.contains(&(c as u8 & 0x1f)), "C-{c} が漏れている");
        }
        assert!(!got.contains(&0x10), "ascii_keys の C-p は対象外");
    }

    #[test]
    fn parses_key_spellings() {
        assert_eq!(parse_key("C-j").unwrap(), Key::Ctrl(0x0a));
        assert_eq!(parse_key("ctrl+g").unwrap(), Key::Ctrl(0x07));
        assert_eq!(parse_key("Ctrl-A").unwrap(), Key::Ctrl(0x01));
        assert_eq!(parse_key("space").unwrap(), Key::Char(' '));
        assert_eq!(parse_key("Enter").unwrap(), Key::Enter);
        assert_eq!(parse_key("esc").unwrap(), Key::Esc);
        assert_eq!(parse_key("bs").unwrap(), Key::Backspace);
        assert_eq!(parse_key("C-space").unwrap(), Key::Ctrl(0x00));
        assert_eq!(parse_key("/").unwrap(), Key::Char('/'));
        assert_eq!(parse_key("Q").unwrap(), Key::Char('Q'));
        assert!(parse_key("C-").is_err());
        assert!(parse_key("C-1").is_err());
        assert!(parse_key("").is_err());
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
