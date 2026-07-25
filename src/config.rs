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

/// 設定ファイルを見張る間隔。
const POLL: Duration = Duration::from_secs(1);

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
    /// 空の見出し語で変換を始める (複合語向け)
    pub start_conversion: Vec<Key>,
    /// ASCII の見出し語で変換する
    pub abbrev: Vec<Key>,
    /// 変換する / 次の候補へ
    pub convert: Vec<Key>,
    /// 前の候補へ
    pub previous: Vec<Key>,
    /// 候補一覧から選ぶキー。並び順がそのまま一覧の並び順になる
    pub select: Vec<char>,
    /// 一覧を出さずに一つずつ送る候補数
    pub inline_candidates: usize,
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
            start_conversion: vec![Key::Char('Q')],
            abbrev: vec![Key::Char('/')],
            convert: vec![Key::Char(' ')],
            previous: vec![Key::Char('x')],
            select: vec!['a', 's', 'd', 'f', 'j', 'k', 'l'],
            inline_candidates: 4,
        }
    }
}

impl Config {
    /// 一覧一頁あたりの候補数。選択キーの数がそのまま頁の大きさになる。
    pub fn page_size(&self) -> usize {
        self.select.len().max(1)
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
                    "start_conversion" => &mut cfg.start_conversion,
                    "abbrev" => &mut cfg.abbrev,
                    "convert" => &mut cfg.convert,
                    "previous" => &mut cfg.previous,
                    "select" => {
                        cfg.select = parse_select(value)?;
                        continue;
                    }
                    other => bail!("keys.{other} は知らない項目"),
                };
                *slot = parse_keys(name, value)?;
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
                    other => bail!("candidates.{other} は知らない項目"),
                }
            }
        }

        for (name, value) in &table {
            if !matches!(name.as_str(), "keys" | "candidates") {
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
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let p = std::env::temp_dir().join("ttyskk-no-such-config-file.toml");
        let _ = std::fs::remove_file(&p);
        assert_eq!(Config::load(&p).unwrap(), Config::default());
    }
}
