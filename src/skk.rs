//! SKK の状態機械。
//!
//! 未確定の文字は一切子プロセスへ送らない。確定した文字列だけを `to_child` に載せ、
//! 途中経過は `preedit()` が返す区間列として重ね描きに回す。

use crate::dict::{Candidate, Dict};
use crate::romaji::{self, Romaji};

/// 一覧を出さずに一つずつ送る候補数。これを超えると横並びの一覧になる。
const INLINE_CANDIDATES: usize = 4;
/// 一覧一頁あたりの候補数。
const PAGE_SIZE: usize = 7;
/// 一覧から候補を選ぶキー。
const SELECT_KEYS: [char; PAGE_SIZE] = ['a', 's', 'd', 'f', 'j', 'k', 'l'];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Ascii,
    Hiragana,
    Katakana,
    ZenkakuAscii,
}

impl Mode {
    fn is_kana(&self) -> bool {
        matches!(self, Mode::Hiragana | Mode::Katakana)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// 変換していない状態。かなはそのまま確定して子へ送る。
    Direct,
    /// ▽ 見出し語を入力中。
    Composing,
    /// ▼ 候補を選択中。
    Selecting,
}

/// 押されたキー。エスケープ列は解釈せず素通しする。
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Char(char),
    Ctrl(u8),
    Enter,
    Backspace,
    Tab,
    Esc,
    Raw(Vec<u8>),
}

/// 重ね描きする区間の見た目。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// ▽ / ▼ の印と見出し語
    Reading,
    /// かなになっていないローマ字
    Romaji,
    /// 選択中の候補
    Candidate,
    /// 候補一覧の項目
    ListItem,
    /// 候補一覧で選択中の項目
    ListSelected,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub style: Style,
    pub text: String,
}

#[derive(Default)]
pub struct Response {
    /// 子プロセスへ流すバイト列。
    pub to_child: Vec<u8>,
    /// 入力モードが変わったか (カーソル色の更新に使う)。
    pub mode_changed: bool,
}

impl Response {
    fn text(s: &str) -> Self {
        Response {
            to_child: s.as_bytes().to_vec(),
            mode_changed: false,
        }
    }
}

pub struct Skk {
    pub mode: Mode,
    phase: Phase,
    romaji: Romaji,
    /// 見出し語。常にひらがなで保持し、表示時にモードへ合わせる。
    reading: String,
    /// 送り仮名のローマ字頭文字。送りあり変換の見出し語に使う。
    okuri_head: Option<char>,
    okuri_kana: String,
    /// `/` で始めた ASCII 見出し語の入力中か。
    abbrev: bool,
    candidates: Vec<Candidate>,
    cand_index: usize,
    /// 変換に使った見出し語 (学習の登録先)。
    dict_key: String,
    dict: Dict,
}

impl Skk {
    pub fn new(dict: Dict) -> Self {
        Skk {
            mode: Mode::Ascii,
            phase: Phase::Direct,
            romaji: Romaji::new(),
            reading: String::new(),
            okuri_head: None,
            okuri_kana: String::new(),
            abbrev: false,
            candidates: Vec::new(),
            cand_index: 0,
            dict_key: String::new(),
            dict,
        }
    }

    pub fn dict_mut(&mut self) -> &mut Dict {
        &mut self.dict
    }

    /// 入力途中の表示。空なら重ね描きするものは無い。
    pub fn preedit(&self) -> Vec<Segment> {
        let mut segs = Vec::new();
        match self.phase {
            Phase::Direct => {
                if !self.romaji.is_empty() {
                    segs.push(Segment {
                        style: Style::Romaji,
                        text: self.romaji.pending().to_string(),
                    });
                }
            }
            Phase::Composing => {
                let mut head = String::from("▽");
                head.push_str(&self.display_reading());
                if self.okuri_head.is_some() {
                    head.push('*');
                    head.push_str(&self.okuri_kana);
                }
                segs.push(Segment {
                    style: Style::Reading,
                    text: head,
                });
                if !self.romaji.is_empty() {
                    segs.push(Segment {
                        style: Style::Romaji,
                        text: self.romaji.pending().to_string(),
                    });
                }
            }
            Phase::Selecting => {
                let cur = self
                    .current_candidate()
                    .map(|c| c.text.clone())
                    .unwrap_or_default();
                segs.push(Segment {
                    style: Style::Candidate,
                    text: format!("▼{}{}", cur, self.okuri_kana),
                });
                if let Some(annot) = self
                    .current_candidate()
                    .and_then(|c| c.annotation.clone())
                    .filter(|_| !self.list_visible())
                {
                    segs.push(Segment {
                        style: Style::ListItem,
                        text: format!(" ; {}", annot),
                    });
                }
                if self.list_visible() {
                    segs.push(Segment {
                        style: Style::ListItem,
                        text: " ".into(),
                    });
                    let (start, end) = self.page_range();
                    for (n, i) in (start..end).enumerate() {
                        let style = if i == self.cand_index {
                            Style::ListSelected
                        } else {
                            Style::ListItem
                        };
                        segs.push(Segment {
                            style,
                            text: format!("{}:{}", SELECT_KEYS[n], self.candidates[i].text),
                        });
                        if i + 1 < end {
                            segs.push(Segment {
                                style: Style::ListItem,
                                text: " ".into(),
                            });
                        }
                    }
                }
            }
        }
        segs
    }

    fn display_reading(&self) -> String {
        if self.abbrev || self.mode != Mode::Katakana {
            self.reading.clone()
        } else {
            romaji::to_katakana(&self.reading)
        }
    }

    fn current_candidate(&self) -> Option<&Candidate> {
        self.candidates.get(self.cand_index)
    }

    fn list_visible(&self) -> bool {
        self.cand_index >= INLINE_CANDIDATES
    }

    fn page_range(&self) -> (usize, usize) {
        let page = (self.cand_index - INLINE_CANDIDATES) / PAGE_SIZE;
        let start = INLINE_CANDIDATES + page * PAGE_SIZE;
        (start, (start + PAGE_SIZE).min(self.candidates.len()))
    }

    fn reset(&mut self) {
        self.phase = Phase::Direct;
        self.romaji.clear();
        self.reading.clear();
        self.okuri_head = None;
        self.okuri_kana.clear();
        self.abbrev = false;
        self.candidates.clear();
        self.cand_index = 0;
        self.dict_key.clear();
    }

    /// かなをモードに合わせて整える。
    fn shape(&self, kana: &str) -> String {
        match self.mode {
            Mode::Katakana => romaji::to_katakana(kana),
            _ => kana.to_string(),
        }
    }

    pub fn handle(&mut self, key: Key) -> Response {
        match self.phase {
            Phase::Direct => self.handle_direct(key),
            Phase::Composing => self.handle_composing(key),
            Phase::Selecting => self.handle_selecting(key),
        }
    }

    // ---- 直接入力 ----

    fn handle_direct(&mut self, key: Key) -> Response {
        // ASCII / 全角英数モードはほぼ素通し
        if !self.mode.is_kana() {
            return match key {
                Key::Ctrl(0x0a) => {
                    self.mode = Mode::Hiragana;
                    Response {
                        mode_changed: true,
                        ..Default::default()
                    }
                }
                Key::Char(c) if self.mode == Mode::ZenkakuAscii => {
                    Response::text(&romaji::to_zenkaku(c).to_string())
                }
                k => Response {
                    to_child: raw_bytes(&k),
                    mode_changed: false,
                },
            };
        }

        match key {
            Key::Ctrl(0x0a) => {
                // C-j: 途中のローマ字を確定させる
                let out = self.romaji.flush();
                Response::text(&self.shape(&out))
            }
            Key::Ctrl(0x07) => {
                // C-g: 途中のローマ字を捨てる
                self.romaji.clear();
                Response::default()
            }
            Key::Backspace => {
                if self.romaji.backspace() {
                    Response::default()
                } else {
                    Response {
                        to_child: vec![0x7f],
                        ..Default::default()
                    }
                }
            }
            Key::Char('l') if self.romaji.is_empty() => {
                self.mode = Mode::Ascii;
                Response {
                    mode_changed: true,
                    ..Default::default()
                }
            }
            Key::Char('L') if self.romaji.is_empty() => {
                self.mode = Mode::ZenkakuAscii;
                Response {
                    mode_changed: true,
                    ..Default::default()
                }
            }
            Key::Char('q') if self.romaji.is_empty() => {
                self.mode = if self.mode == Mode::Hiragana {
                    Mode::Katakana
                } else {
                    Mode::Hiragana
                };
                Response {
                    mode_changed: true,
                    ..Default::default()
                }
            }
            Key::Char('Q') => {
                self.phase = Phase::Composing;
                self.romaji.clear();
                Response::default()
            }
            Key::Char('/') if self.romaji.is_empty() => {
                self.phase = Phase::Composing;
                self.abbrev = true;
                Response::default()
            }
            Key::Char(c) if c.is_ascii_uppercase() => {
                self.phase = Phase::Composing;
                let kana = self.romaji.feed(c.to_ascii_lowercase());
                self.reading.push_str(&kana);
                Response::default()
            }
            Key::Char(c) => {
                let out = self.romaji.feed(c);
                Response::text(&self.shape(&out))
            }
            k => {
                // 制御キーや矢印は素通し。途中のローマ字は先に確定させる。
                let flushed = self.romaji.flush();
                let mut bytes = self.shape(&flushed).into_bytes();
                bytes.extend(raw_bytes(&k));
                Response {
                    to_child: bytes,
                    mode_changed: false,
                }
            }
        }
    }

    // ---- ▽ 見出し語入力 ----

    fn handle_composing(&mut self, key: Key) -> Response {
        match key {
            Key::Ctrl(0x07) => {
                // C-g: 取り消して何も出さない
                self.reset();
                Response::default()
            }
            Key::Ctrl(0x0a) | Key::Enter => {
                let text = self.confirm_reading();
                self.reset();
                Response::text(&text)
            }
            Key::Backspace => {
                if self.romaji.backspace() {
                } else if !self.okuri_kana.is_empty() {
                    self.okuri_kana.pop();
                } else if self.okuri_head.is_some() {
                    self.okuri_head = None;
                } else if self.reading.pop().is_none() {
                    self.reset();
                }
                Response::default()
            }
            Key::Char(' ') => {
                self.start_conversion();
                Response::default()
            }
            Key::Char('q') if self.okuri_head.is_none() && !self.abbrev => {
                // q: 見出し語をカタカナ (カタカナモードならひらがな) にして確定
                let flushed = self.romaji.flush();
                self.reading.push_str(&flushed);
                let text = if self.mode == Mode::Katakana {
                    romaji::to_hiragana(&self.reading)
                } else {
                    romaji::to_katakana(&self.reading)
                };
                self.reset();
                Response::text(&text)
            }
            Key::Char(c) if self.abbrev => {
                self.reading.push(c);
                Response::default()
            }
            Key::Char(c) if c.is_ascii_uppercase() && !self.reading.is_empty() => {
                // 送り仮名の始まり
                if self.okuri_head.is_none() {
                    self.okuri_head = Some(c.to_ascii_lowercase());
                }
                let kana = self.romaji.feed(c.to_ascii_lowercase());
                if !kana.is_empty() {
                    self.okuri_kana.push_str(&kana);
                    self.start_conversion();
                }
                Response::default()
            }
            Key::Char(c) if c.is_ascii_uppercase() => {
                let kana = self.romaji.feed(c.to_ascii_lowercase());
                self.reading.push_str(&kana);
                Response::default()
            }
            Key::Char(c) => {
                let kana = self.romaji.feed(c);
                if self.okuri_head.is_some() {
                    self.okuri_kana.push_str(&kana);
                    if !self.okuri_kana.is_empty() && self.romaji.is_empty() {
                        self.start_conversion();
                    }
                } else {
                    self.reading.push_str(&kana);
                }
                Response::default()
            }
            k => {
                // 想定外のキーは見出し語を確定してから素通しする
                let text = self.confirm_reading();
                self.reset();
                let mut bytes = text.into_bytes();
                bytes.extend(raw_bytes(&k));
                Response {
                    to_child: bytes,
                    mode_changed: false,
                }
            }
        }
    }

    /// ▽ の内容をそのまま (かなのまま) 確定させた文字列。
    fn confirm_reading(&mut self) -> String {
        let flushed = self.romaji.flush();
        if self.okuri_head.is_some() {
            self.okuri_kana.push_str(&flushed);
        } else {
            self.reading.push_str(&flushed);
        }
        let body = if self.abbrev {
            self.reading.clone()
        } else {
            self.shape(&self.reading)
        };
        format!("{}{}", body, self.okuri_kana)
    }

    fn start_conversion(&mut self) {
        let flushed = self.romaji.flush();
        if self.okuri_head.is_some() {
            self.okuri_kana.push_str(&flushed);
        } else {
            self.reading.push_str(&flushed);
        }
        if self.reading.is_empty() {
            return;
        }
        self.dict_key = match self.okuri_head {
            Some(h) => format!("{}{}", self.reading, h),
            None => self.reading.clone(),
        };
        self.candidates = self.dict.lookup(&self.dict_key);
        if self.candidates.is_empty() {
            // 候補が無ければ ▽ のまま。C-j でかな確定できる。
            return;
        }
        self.cand_index = 0;
        self.phase = Phase::Selecting;
    }

    // ---- ▼ 候補選択 ----

    fn handle_selecting(&mut self, key: Key) -> Response {
        match key {
            Key::Ctrl(0x07) => {
                self.phase = Phase::Composing;
                self.candidates.clear();
                self.cand_index = 0;
                Response::default()
            }
            Key::Ctrl(0x0a) | Key::Enter => {
                let text = self.commit_candidate();
                Response::text(&text)
            }
            Key::Char(' ') => {
                self.next_candidate();
                Response::default()
            }
            Key::Char('x') => {
                self.prev_candidate();
                Response::default()
            }
            Key::Char(c) if self.list_visible() && SELECT_KEYS.contains(&c) => {
                let (start, end) = self.page_range();
                let n = SELECT_KEYS.iter().position(|&k| k == c).unwrap();
                if start + n < end {
                    self.cand_index = start + n;
                    let text = self.commit_candidate();
                    return Response::text(&text);
                }
                Response::default()
            }
            Key::Backspace => {
                self.prev_candidate();
                Response::default()
            }
            Key::Char(c) => {
                // 候補を確定してから、その文字を新しい入力として処理する
                let text = self.commit_candidate();
                let mut r = self.handle(Key::Char(c));
                let mut bytes = text.into_bytes();
                bytes.append(&mut r.to_child);
                Response {
                    to_child: bytes,
                    mode_changed: r.mode_changed,
                }
            }
            k => {
                let text = self.commit_candidate();
                let mut bytes = text.into_bytes();
                bytes.extend(raw_bytes(&k));
                Response {
                    to_child: bytes,
                    mode_changed: false,
                }
            }
        }
    }

    fn next_candidate(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        if self.cand_index + 1 < INLINE_CANDIDATES {
            self.cand_index += 1;
            return;
        }
        if !self.list_visible() {
            // 一覧の表示を始める
            if self.candidates.len() > INLINE_CANDIDATES {
                self.cand_index = INLINE_CANDIDATES;
            }
            return;
        }
        // 一覧が出ているときは頁単位で送る
        let (_, end) = self.page_range();
        if end < self.candidates.len() {
            self.cand_index = end;
        }
    }

    fn prev_candidate(&mut self) {
        if self.cand_index == 0 {
            self.phase = Phase::Composing;
            self.candidates.clear();
            return;
        }
        if !self.list_visible() {
            self.cand_index -= 1;
            return;
        }
        let (start, _) = self.page_range();
        self.cand_index = start.saturating_sub(PAGE_SIZE).max(INLINE_CANDIDATES);
        if start == INLINE_CANDIDATES {
            self.cand_index = INLINE_CANDIDATES - 1;
        }
    }

    fn commit_candidate(&mut self) -> String {
        let text = match self.current_candidate() {
            Some(c) => {
                let cand = c.clone();
                self.dict.learn(&self.dict_key.clone(), &cand);
                format!("{}{}", cand.text, self.okuri_kana)
            }
            None => String::new(),
        };
        self.reset();
        text
    }
}

/// 解釈しないキーを元のバイト列に戻す。
fn raw_bytes(k: &Key) -> Vec<u8> {
    match k {
        Key::Char(c) => c.to_string().into_bytes(),
        Key::Ctrl(b) => vec![*b],
        Key::Enter => vec![0x0d],
        Key::Backspace => vec![0x7f],
        Key::Tab => vec![0x09],
        Key::Esc => vec![0x1b],
        Key::Raw(v) => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 試験は並列に走るので、辞書ファイルは呼び出しごとに別の場所に作る。
    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn skk_with(entries: &[(&str, &str)]) -> Skk {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ttyskk-test-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = dir.join("sys.dict");
        let mut body = String::from(";; okuri-nasi entries.\n");
        for (k, v) in entries {
            body.push_str(&format!("{} {}\n", k, v));
        }
        std::fs::write(&sys, body).unwrap();
        let dict = Dict::load(&[sys], dir.join("user.dict"), None).unwrap();
        Skk::new(dict)
    }

    fn typed(skk: &mut Skk, s: &str) -> String {
        let mut out = Vec::new();
        for c in s.chars() {
            let key = match c {
                '\n' => Key::Ctrl(0x0a),
                '\x07' => Key::Ctrl(0x07),
                '\x7f' => Key::Backspace,
                c => Key::Char(c),
            };
            out.extend(skk.handle(key).to_child);
        }
        String::from_utf8(out).unwrap()
    }

    fn preedit_text(skk: &Skk) -> String {
        skk.preedit().into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn ascii_passes_through() {
        let mut skk = skk_with(&[]);
        assert_eq!(typed(&mut skk, "ls -la"), "ls -la");
    }

    #[test]
    fn kana_mode_emits_kana() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, "aiueo"), "あいうえお");
    }

    #[test]
    fn pending_romaji_is_not_sent() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, "k"), "");
        assert_eq!(preedit_text(&skk), "k");
        assert_eq!(typed(&mut skk, "a"), "か");
        assert_eq!(preedit_text(&skk), "");
    }

    #[test]
    fn conversion_without_okuri() {
        let mut skk = skk_with(&[("かんじ", "/漢字/幹事/")]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, "Kanji"), "");
        assert_eq!(preedit_text(&skk), "▽かんじ");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼漢字");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼幹事");
        assert_eq!(typed(&mut skk, "\n"), "幹事");
    }

    #[test]
    fn conversion_with_okuri() {
        let mut skk = skk_with(&[("うごk", "/動/")]);
        skk.handle(Key::Ctrl(0x0a));
        // UgoKu → 「動く」
        typed(&mut skk, "Ugo");
        assert_eq!(preedit_text(&skk), "▽うご");
        typed(&mut skk, "Ku");
        assert_eq!(preedit_text(&skk), "▼動く");
        assert_eq!(typed(&mut skk, "\n"), "動く");
    }

    #[test]
    fn learning_moves_candidate_to_front() {
        let mut skk = skk_with(&[("かんじ", "/漢字/幹事/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji  \n");
        // 二回目は学習した「幹事」が先頭に出る
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼幹事");
    }

    #[test]
    fn cancel_discards_everything() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(typed(&mut skk, "\x07"), "");
        assert_eq!(preedit_text(&skk), "▽かんじ");
        assert_eq!(typed(&mut skk, "\x07"), "");
        assert_eq!(preedit_text(&skk), "");
    }

    #[test]
    fn katakana_toggle() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "q");
        assert_eq!(typed(&mut skk, "aiu"), "アイウ");
    }

    #[test]
    fn q_in_composing_makes_katakana() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, "Aiuq"), "アイウ");
    }

    #[test]
    fn candidate_list_appears_after_four() {
        let mut skk = skk_with(&[("あ", "/亜/唖/娃/阿/哀/愛/挨/姶/逢/葵/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "A     ");
        let text = preedit_text(&skk);
        // 5 番目以降が一覧になる。a=哀 s=愛 d=挨 …
        assert!(text.contains("a:哀"), "一覧が出ていない: {text}");
        assert!(text.contains("d:挨"), "一覧が短い: {text}");
        assert_eq!(typed(&mut skk, "d"), "挨");
    }

    #[test]
    fn backspace_walks_back_through_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "▽かん");
    }

    /// herdr の prefix (C-z) が横取りされずに子へ届くこと。作った動機そのもの。
    #[test]
    fn control_keys_reach_the_child() {
        let mut skk = skk_with(&[]);
        assert_eq!(skk.handle(Key::Ctrl(0x1a)).to_child, vec![0x1a]);
        skk.handle(Key::Ctrl(0x0a)); // かなモードへ
        assert_eq!(skk.handle(Key::Ctrl(0x1a)).to_child, vec![0x1a]);
        // 入力途中のローマ字があっても、確定させたうえで届く
        skk.handle(Key::Char('k'));
        let r = skk.handle(Key::Ctrl(0x1a));
        assert_eq!(r.to_child, vec![0x1a]);
    }

    /// 変換中に矢印キーなどが来たら、見出し語を確定してから素通しする。
    #[test]
    fn escape_sequence_confirms_then_passes() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        let r = skk.handle(Key::Raw(b"\x1b[A".to_vec()));
        assert_eq!(r.to_child, "かんじ\x1b[A".as_bytes());
        assert_eq!(preedit_text(&skk), "");
    }

    #[test]
    fn zenkaku_mode() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "L");
        assert_eq!(typed(&mut skk, "abc"), "ａｂｃ");
    }
}
