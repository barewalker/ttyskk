//! SKK の状態機械。
//!
//! 未確定の文字は一切子プロセスへ送らない。確定した文字列だけを `to_child` に載せ、
//! 途中経過は `preedit()` が返す区間列として重ね描きに回す。

use crate::config::{Config, Layout, Marker};
use crate::dict::{Candidate, Dict};
use crate::num;
use crate::romaji::{self, Romaji};

/// TAB 補完で拾う見出し語の上限。多すぎると巡るのに手間がかかる。
const COMPLETIONS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Ascii,
    Hiragana,
    Katakana,
    HankakuKatakana,
    ZenkakuAscii,
}

impl Mode {
    fn is_kana(&self) -> bool {
        matches!(
            self,
            Mode::Hiragana | Mode::Katakana | Mode::HankakuKatakana
        )
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
    /// モードの印。色でモードを表すので、モードごとに分かれている。
    ModeHiragana,
    ModeKatakana,
    ModeHankaku,
    ModeZenkaku,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub style: Style,
    pub text: String,
}

/// カーソルのそばに敷く色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tint {
    pub style: Style,
    /// カーソルからの桁のずれ
    pub offset: usize,
    /// 敷く文字。`None` なら控えの文字をそのまま使う (下の文字を隠さない)。
    pub glyph: Option<char>,
}

/// 重ね描きするもの。
///
/// 候補一覧を浮かせる設定では、一覧だけがカーソルから離れた行に出る。
/// 描く場所が違うので受け渡しの段階で分けておく。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preedit {
    /// カーソル位置から描く
    pub at_cursor: Vec<Segment>,
    /// 別の行に一行で浮かせる (空なら無し)
    pub floating: Vec<Segment>,
    /// セルに敷く色。
    pub cursor_tint: Option<Tint>,
}

impl Preedit {
    pub fn is_empty(&self) -> bool {
        self.at_cursor.is_empty() && self.floating.is_empty() && self.cursor_tint.is_none()
    }
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

/// 辞書登録の途中。候補が見つからないときに積む。
///
/// 入れ子にできる。登録内容を打っている最中にさらに未知語を変換した場合、
/// もう一段積まれる。
struct Registration {
    /// 辞書に登録する見出し語 (送りありなら `うごk`)
    key: String,
    /// 打ち込み中の登録内容
    buffer: String,
    /// 取り消したときに ▽ へ戻すための控え
    reading: String,
    okuri_head: Option<char>,
    okuri_kana: String,
    abbrev: bool,
}

impl Registration {
    /// 画面に出す見出し (送りありは `うご*く`)。
    fn label(&self) -> String {
        match self.okuri_head {
            Some(_) => format!("{}*{}", self.reading, self.okuri_kana),
            None => self.reading.clone(),
        }
    }
}

/// 選べる候補ひとつ。学習と削除の宛先を候補ごとに覚えておく。
///
/// 数値変換では「だい5かい」を `だい#かい` として引くので、辞書に書き戻す先が
/// 打った見出し語と食い違う。候補の本文も `第#1回` のままで持ち、画面に出すとき
/// と子へ送るときだけ数字を戻す。こうしないと `だい#かい /第５回/` のように、
/// その数字専用の項目を辞書へ書いてしまう。
struct Choice {
    /// 学習・削除の宛先
    key: String,
    cand: Candidate,
}

/// TAB 補完の途中。見出し語を書き換えてしまうので、元に戻せるようにしておく。
struct Completion {
    /// 補完を始めたときの見出し語
    original: String,
    words: Vec<String>,
    index: usize,
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
    candidates: Vec<Choice>,
    cand_index: usize,
    /// 見出し語から取り出した数字 (数値変換で `#` に戻す)
    numbers: Vec<String>,
    /// 変換に使った見出し語 (学習の登録先)。
    dict_key: String,
    dict: Dict,
    cfg: Config,
    /// 辞書登録の積み上げ。空でなければ登録中。
    regs: Vec<Registration>,
    /// TAB 補完の途中。見出し語が変わったら捨てる。
    completion: Option<Completion>,
    /// 直前に確定した変換 (見出し語, 出力)。合成語の学習に使う。
    ///
    /// 直接入力で何か文字を出したら捨てる。接頭辞と次の語が画面上で隣り合って
    /// いることが繋げる条件なので (ddskk は `looking-at` で同じことを確かめる)。
    last_commit: Option<(String, String)>,
}

impl Skk {
    pub fn new(dict: Dict, cfg: Config) -> Self {
        Skk {
            cfg,
            mode: Mode::Ascii,
            phase: Phase::Direct,
            romaji: Romaji::new(),
            reading: String::new(),
            okuri_head: None,
            okuri_kana: String::new(),
            abbrev: false,
            candidates: Vec::new(),
            cand_index: 0,
            numbers: Vec::new(),
            dict_key: String::new(),
            dict,
            regs: Vec::new(),
            completion: None,
            last_commit: None,
        }
    }

    /// モードの印の出し方 (カーソルの見た目を決めるのに使う)。
    pub fn marker(&self) -> Marker {
        self.cfg.mode_marker
    }

    pub fn dict_mut(&mut self) -> &mut Dict {
        &mut self.dict
    }

    /// 設定ファイルが書き換わったときに差し替える。
    ///
    /// 変換の途中で入れ替わっても状態は壊れない。見出し語や候補は設定に依らず、
    /// 参照するのは次のキーを解釈するときだけだから。一覧の頁の大きさが変わると
    /// 表示中の頁の切れ目は変わるが、選んでいる候補そのものはずれない。
    pub fn set_config(&mut self, cfg: Config) {
        self.cfg = cfg;
    }

    /// 入力途中の表示。空なら重ね描きするものは無い。
    pub fn preedit(&self) -> Preedit {
        let mut segs = Vec::new();
        let mut floating = Vec::new();
        // 登録中は見出しと打ち込み済みの内容を前に置く。入れ子は括弧の重なりで表す。
        if let Some(reg) = self.regs.last() {
            let depth = self.regs.len();
            segs.push(Segment {
                style: Style::ListItem,
                text: format!(
                    "{}登録:{}{}",
                    "[".repeat(depth),
                    reg.label(),
                    "]".repeat(depth)
                ),
            });
            if !reg.buffer.is_empty() {
                segs.push(Segment {
                    style: Style::Reading,
                    text: reg.buffer.clone(),
                });
            }
        }
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
                    .map(|c| self.shown(c))
                    .unwrap_or_default();
                segs.push(Segment {
                    style: Style::Candidate,
                    text: format!("▼{}{}", cur, self.okuri_kana),
                });
                if let Some(annot) = self
                    .current_candidate()
                    .and_then(|c| c.cand.annotation.clone())
                    .filter(|_| !self.list_visible())
                {
                    segs.push(Segment {
                        style: Style::ListItem,
                        text: format!(" ; {}", annot),
                    });
                }
                if self.list_visible() {
                    let list = &mut match self.cfg.layout {
                        Layout::Inline => &mut segs,
                        Layout::Float => &mut floating,
                    };
                    if self.cfg.layout == Layout::Inline {
                        list.push(Segment {
                            style: Style::ListItem,
                            text: " ".into(),
                        });
                    }
                    let (start, end) = self.page_range();
                    for (n, i) in (start..end).enumerate() {
                        let style = if i == self.cand_index {
                            Style::ListSelected
                        } else {
                            Style::ListItem
                        };
                        list.push(Segment {
                            style,
                            text: format!(
                                "{}:{}",
                                self.cfg.select[n],
                                self.shown(&self.candidates[i])
                            ),
                        });
                        if i + 1 < end {
                            list.push(Segment {
                                style: Style::ListItem,
                                text: " ".into(),
                            });
                        }
                    }
                    // 残りの件数を添える (ddskk と同じ)
                    let rest = self.candidates.len() - end;
                    if rest > 0 {
                        list.push(Segment {
                            style: Style::ListItem,
                            text: format!("  [残り {rest}]"),
                        });
                    }
                }
            }
        }
        let mode_style = match self.mode {
            Mode::Ascii => None,
            Mode::Hiragana => Some(Style::ModeHiragana),
            Mode::Katakana => Some(Style::ModeKatakana),
            Mode::HankakuKatakana => Some(Style::ModeHankaku),
            Mode::ZenkakuAscii => Some(Style::ModeZenkaku),
        };
        let mut cursor_tint = None;
        match self.cfg.mode_marker {
            Marker::Off => {}
            // カーソル位置のセルに色を敷く。文字を足さないので邪魔にならない。
            // 打ち込み中の文字がある間はその先頭が同じ場所に来るので出さない。
            Marker::Cell | Marker::Symbol | Marker::Beside => {
                // 打ち込み中の文字がある間はその先頭が同じ場所に来るので出さない
                if segs.is_empty() {
                    let offset = usize::from(self.cfg.mode_marker == Marker::Beside);
                    // 記号を出す方式では、色に頼らずモードが分かるようにする
                    let glyph =
                        (self.cfg.mode_marker == Marker::Symbol).then_some(match self.mode {
                            Mode::Hiragana => self.cfg.mode_symbols[0],
                            Mode::Katakana => self.cfg.mode_symbols[1],
                            Mode::HankakuKatakana => self.cfg.mode_symbols[2],
                            _ => self.cfg.mode_symbols[3],
                        });
                    cursor_tint = mode_style.map(|style| Tint {
                        style,
                        offset,
                        glyph,
                    });
                }
            }
            // 印を末尾に置く。カーソルは重ね描きの先頭に戻るので、
            // 印は打ち込み中の文字より後ろに出る。
            Marker::Letter => {
                if let Some(style) = mode_style {
                    let text = match self.mode {
                        Mode::Hiragana => "あ",
                        Mode::Katakana => "ア",
                        Mode::HankakuKatakana => "半",
                        _ => "Ａ",
                    };
                    segs.push(Segment {
                        style,
                        text: text.to_string(),
                    });
                }
            }
        }

        Preedit {
            at_cursor: segs,
            floating,
            cursor_tint,
        }
    }

    fn display_reading(&self) -> String {
        if self.abbrev {
            return self.reading.clone();
        }
        self.shape(&self.reading)
    }

    fn current_candidate(&self) -> Option<&Choice> {
        self.candidates.get(self.cand_index)
    }

    /// 候補の本文を、画面と出力に出す形にする。
    ///
    /// 数字を戻し、接頭辞・接尾辞の印を落とす。`SKK-JISYO.L` では候補側に `>` が
    /// 付くのは 1 件だけだが、skkeleton も同じ処理を持つ。
    fn shown(&self, c: &Choice) -> String {
        let t = num::expand(&c.cand.text, &self.numbers);
        if c.key.ends_with('>') {
            t.trim_end_matches('>').to_string()
        } else if c.key.starts_with('>') {
            t.trim_start_matches('>').to_string()
        } else {
            t
        }
    }

    /// 確定を記録し、接頭辞・接尾辞に続いた場合は繋げて覚える。
    ///
    /// ddskk の `skk-learn-combined-word` と同じ。「さい>」→再 のあと
    /// 「りよう」→利用 と確定したら `さいりよう /再利用/` を覚える。
    /// 送りありの語は繋げない (候補が語幹だけなので繋いでも筋が通らない)。
    fn note_commit(&mut self, key: String, text: String, okuri: bool) {
        let prev = self.last_commit.take();
        if self.cfg.learn_combined
            && !okuri
            && let Some((pk, pt)) = prev
        {
            let joined = if pk.ends_with('>') && !key.ends_with('>') && !key.starts_with('>') {
                // 接頭辞のあとに普通の語
                Some((
                    format!("{}{}", &pk[..pk.len() - 1], key),
                    format!("{pt}{text}"),
                ))
            } else if key.starts_with('>') && !pk.starts_with('>') && !pk.ends_with('>') {
                // 普通の語のあとに接尾辞
                Some((format!("{}{}", pk, &key[1..]), format!("{pt}{text}")))
            } else {
                None
            };
            if let Some((k, joined_text)) = joined
                && !joined_text.is_empty()
            {
                self.dict.learn(
                    &k,
                    &Candidate {
                        text: joined_text,
                        annotation: None,
                    },
                );
            }
        }
        if okuri {
            self.last_commit = None;
        } else {
            self.last_commit = Some((key, text));
        }
    }

    fn list_visible(&self) -> bool {
        self.cand_index >= self.cfg.inline_candidates
    }

    fn page_range(&self) -> (usize, usize) {
        let inline = self.cfg.inline_candidates;
        let size = self.cfg.page_size();
        let page = (self.cand_index - inline) / size;
        let start = inline + page * size;
        (start, (start + size).min(self.candidates.len()))
    }

    fn reset(&mut self) {
        self.completion = None;
        self.phase = Phase::Direct;
        self.romaji.clear();
        self.reading.clear();
        self.okuri_head = None;
        self.okuri_kana.clear();
        self.abbrev = false;
        self.candidates.clear();
        self.cand_index = 0;
        self.numbers.clear();
        self.dict_key.clear();
    }

    /// かなをモードに合わせて整える。
    fn shape(&self, kana: &str) -> String {
        match self.mode {
            Mode::Katakana => romaji::to_katakana(kana),
            Mode::HankakuKatakana => romaji::to_hankaku_katakana(kana),
            _ => kana.to_string(),
        }
    }

    pub fn handle(&mut self, key: Key) -> Response {
        if self.regs.is_empty() {
            // 挿入モードを抜けたときにかなが残らないようにする。vim / nvim で
            // Esc を押すと挿入モードを抜けるので、同じキーで ASCII へ戻す。
            // 登録の途中では効かせない (打ち込みの最中に消えると困る)。
            if self.mode != Mode::Ascii && self.cfg.ascii_keys.contains(&key) {
                let mut r = self.dispatch(key);
                self.mode = Mode::Ascii;
                r.mode_changed = true;
                return r;
            }
            return self.dispatch(key);
        }
        // 登録中。子へ出るはずだった文字は登録内容に溜める。
        if matches!(key, Key::Raw(_)) {
            // 矢印などは受け付けない。挟むと登録内容が壊れる。
            return Response::default();
        }
        if self.phase == Phase::Direct {
            if key == Key::Enter {
                return self.finish_registration();
            }
            if self.romaji.is_empty() {
                if self.cfg.cancel.contains(&key) {
                    return self.abort_registration();
                }
                if key == Key::Backspace {
                    let reg = self.regs.last_mut().expect("登録中");
                    if reg.buffer.pop().is_none() {
                        return self.abort_registration();
                    }
                    return Response::default();
                }
            }
        }
        let r = self.dispatch(key);
        self.capture(r)
    }

    /// 子へ出るはずだった文字を、いちばん内側の登録内容へ回す。
    ///
    /// 制御文字は捨てる。割り当ての無い `C-z` などがそのまま混ざると、
    /// 辞書に制御文字を含む項目ができてしまうため。
    fn capture(&mut self, r: Response) -> Response {
        let Some(reg) = self.regs.last_mut() else {
            return r;
        };
        if let Ok(s) = String::from_utf8(r.to_child) {
            reg.buffer.extend(s.chars().filter(|c| !c.is_control()));
        }
        Response {
            to_child: Vec::new(),
            mode_changed: r.mode_changed,
        }
    }

    fn dispatch(&mut self, key: Key) -> Response {
        match self.phase {
            Phase::Direct => self.handle_direct(key),
            Phase::Composing => self.handle_composing(key),
            Phase::Selecting => self.handle_selecting(key),
        }
    }

    /// 候補が尽きたので辞書登録を始める。
    fn begin_registration(&mut self) {
        self.regs.push(Registration {
            key: std::mem::take(&mut self.dict_key),
            buffer: String::new(),
            reading: self.reading.clone(),
            okuri_head: self.okuri_head,
            okuri_kana: self.okuri_kana.clone(),
            abbrev: self.abbrev,
        });
        self.reset();
    }

    /// 登録を確定する。空のまま確定したときは登録せず ▽ へ戻す。
    fn finish_registration(&mut self) -> Response {
        let flushed = self.romaji.flush();
        let shaped = self.shape(&flushed);
        if let Some(reg) = self.regs.last_mut() {
            reg.buffer.push_str(&shaped);
        }
        let reg = self.regs.pop().expect("登録中");
        if reg.buffer.is_empty() {
            return self.resume_composing(reg);
        }
        let cand = Candidate {
            text: reg.buffer.clone(),
            annotation: None,
        };
        self.dict.learn(&reg.key, &cand);
        let text = format!("{}{}", reg.buffer, reg.okuri_kana);
        self.capture(Response::text(&text))
    }

    /// 登録を取り消して ▽ に戻す。打ちかけの登録内容は捨てる。
    fn abort_registration(&mut self) -> Response {
        let reg = self.regs.pop().expect("登録中");
        self.resume_composing(reg)
    }

    fn resume_composing(&mut self, reg: Registration) -> Response {
        self.reset();
        self.phase = Phase::Composing;
        self.reading = reg.reading;
        self.okuri_head = reg.okuri_head;
        self.okuri_kana = reg.okuri_kana;
        self.abbrev = reg.abbrev;
        Response::default()
    }

    // ---- 直接入力 ----

    fn handle_direct(&mut self, key: Key) -> Response {
        let r = self.direct_inner(key);
        // 何か出したなら、接頭辞と次の語はもう隣り合っていない
        if !r.to_child.is_empty() {
            self.last_commit = None;
        }
        r
    }

    fn direct_inner(&mut self, key: Key) -> Response {
        // ASCII / 全角英数モードはほぼ素通し
        if !self.mode.is_kana() {
            return match key {
                k if self.cfg.kana.contains(&k) => {
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
            k if self.cfg.confirm.contains(&k) => {
                // 途中のローマ字を確定させる
                let out = self.romaji.flush();
                Response::text(&self.shape(&out))
            }
            k if self.cfg.cancel.contains(&k) => {
                // 途中のローマ字を捨てる
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
            k if self.romaji.is_empty() && self.cfg.ascii.contains(&k) => {
                self.mode = Mode::Ascii;
                Response {
                    mode_changed: true,
                    ..Default::default()
                }
            }
            k if self.romaji.is_empty() && self.cfg.zenkaku.contains(&k) => {
                self.mode = Mode::ZenkakuAscii;
                Response {
                    mode_changed: true,
                    ..Default::default()
                }
            }
            k if self.romaji.is_empty() && self.cfg.hankaku_katakana.contains(&k) => {
                self.mode = if self.mode == Mode::HankakuKatakana {
                    Mode::Hiragana
                } else {
                    Mode::HankakuKatakana
                };
                Response {
                    mode_changed: true,
                    ..Default::default()
                }
            }
            k if self.romaji.is_empty() && self.cfg.katakana.contains(&k) => {
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
            k if self.cfg.start_conversion.contains(&k) => {
                self.phase = Phase::Composing;
                self.romaji.clear();
                Response::default()
            }
            k if self.romaji.is_empty() && self.cfg.abbrev.contains(&k) => {
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
        // 見出し語が変われば補完の並びは意味を失う。補完そのものと取り消し
        // (元へ戻す) 以外のキーでは捨てる。
        let keep = self.cfg.complete.contains(&key)
            || self.cfg.complete_previous.contains(&key)
            || self.cfg.cancel.contains(&key);
        let r = self.composing_inner(key);
        if !keep {
            self.completion = None;
        }
        r
    }

    fn composing_inner(&mut self, key: Key) -> Response {
        match key {
            k if self.cfg.cancel.contains(&k) => {
                // 補完の途中なら、まず補完を取り消して元の見出し語に戻す
                if let Some(c) = self.completion.take() {
                    self.reading = c.original;
                    return Response::default();
                }
                // 取り消して何も出さない
                self.reset();
                Response::default()
            }
            // Enter は割り当てに依らず確定として扱う。端末では改行が「コマンドの
            // 実行」を意味するので、変換の途中で子へ送るわけにいかない。
            Key::Enter => {
                let text = self.confirm_reading();
                self.reset();
                Response::text(&text)
            }
            k if self.cfg.confirm.contains(&k) => {
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
            k if self.cfg.affix.contains(&k)
                && !self.reading.is_empty()
                && self.okuri_head.is_none()
                && !self.abbrev =>
            {
                // 接頭辞。見出し語の末尾に `>` を足してすぐ変換する。
                let flushed = self.romaji.flush();
                self.reading.push_str(&flushed);
                self.reading.push('>');
                self.start_conversion();
                Response::default()
            }
            k if self.cfg.complete.contains(&k) => {
                self.step_completion(1);
                Response::default()
            }
            k if self.cfg.complete_previous.contains(&k) => {
                self.step_completion(-1);
                Response::default()
            }
            k if self.cfg.convert.contains(&k) => {
                self.completion = None;
                self.start_conversion();
                Response::default()
            }
            k if !self.abbrev && self.cfg.hankaku_katakana.contains(&k) => {
                // 見出し語を半角カタカナにして確定する
                let flushed = self.romaji.flush();
                self.reading.push_str(&flushed);
                let text = format!(
                    "{}{}",
                    romaji::to_hankaku_katakana(&self.reading),
                    romaji::to_hankaku_katakana(&self.okuri_kana)
                );
                self.reset();
                Response::text(&text)
            }
            k if self.okuri_head.is_none() && !self.abbrev && self.cfg.katakana.contains(&k) => {
                // 見出し語をカタカナ (カタカナモードならひらがな) にして確定
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

    /// 見出し語を前方一致で補完する。`dir` は 1 で次、-1 で前。
    ///
    /// 補完の元は利用者辞書が先、共有辞書が後ろ。実際に使った語のほうが当たり
    /// やすいため。一度作った並びは見出し語が変わるまで使い回す。
    fn step_completion(&mut self, dir: i32) {
        if self.abbrev {
            return;
        }
        if self.completion.is_none() {
            // 途中のローマ字を確定してから探す
            let flushed = self.romaji.flush();
            self.reading.push_str(&flushed);
            let words = self.dict.complete(&self.reading, COMPLETIONS);
            if words.is_empty() {
                return;
            }
            self.completion = Some(Completion {
                original: std::mem::take(&mut self.reading),
                words,
                index: 0,
            });
        } else if let Some(c) = self.completion.as_mut() {
            let n = c.words.len() as i32;
            c.index = ((c.index as i32 + dir).rem_euclid(n)) as usize;
        }
        if let Some(c) = self.completion.as_ref() {
            self.reading = c.words[c.index].clone();
        }
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
        // まず打った通りに引く。学習も削除もこの見出し語が宛先になる。
        self.candidates = self
            .dict
            .lookup(&self.dict_key)
            .into_iter()
            .map(|cand| Choice {
                key: self.dict_key.clone(),
                cand,
            })
            .collect();
        // 数字を含むなら `#` に置き換えた見出し語でも引く (だい5かい → だい#かい)
        let (abstracted, numbers) = num::abstract_numbers(&self.dict_key);
        self.numbers = numbers;
        if abstracted != self.dict_key {
            for cand in self.dict.lookup(&abstracted) {
                if !self.candidates.iter().any(|c| c.cand.text == cand.text) {
                    self.candidates.push(Choice {
                        key: abstracted.clone(),
                        cand,
                    });
                }
            }
        }
        if self.candidates.is_empty() {
            // 候補が無ければ辞書登録へ移る
            self.begin_registration();
            return;
        }
        self.cand_index = 0;
        self.phase = Phase::Selecting;
    }

    // ---- ▼ 候補選択 ----

    fn handle_selecting(&mut self, key: Key) -> Response {
        match key {
            k if self.cfg.cancel.contains(&k) => {
                self.phase = Phase::Composing;
                self.candidates.clear();
                self.cand_index = 0;
                Response::default()
            }
            // Enter を確定に固定する理由は handle_composing と同じ
            Key::Enter => {
                let text = self.commit_candidate();
                Response::text(&text)
            }
            k if self.cfg.confirm.contains(&k) => {
                let text = self.commit_candidate();
                Response::text(&text)
            }
            k if self.cfg.affix.contains(&k) => {
                // 接尾辞。いまの候補を確定し、`>` から始まる新しい見出し語を立てる。
                let text = self.commit_candidate();
                self.phase = Phase::Composing;
                self.reading = ">".into();
                Response::text(&text)
            }
            k if self.cfg.purge.contains(&k) => {
                self.purge_candidate();
                Response::default()
            }
            k if self.cfg.convert.contains(&k) => {
                if self.next_candidate() {
                    // 候補を出し切ったので辞書登録へ
                    self.begin_registration();
                }
                Response::default()
            }
            k if self.cfg.previous.contains(&k) => {
                self.prev_candidate();
                Response::default()
            }
            Key::Char(c) if self.list_visible() && self.cfg.select.contains(&c) => {
                let (start, end) = self.page_range();
                let n = self.cfg.select.iter().position(|&k| k == c).unwrap();
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
                let mut r = self.dispatch(Key::Char(c));
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

    /// 次の候補へ。もう先が無ければ true (辞書登録へ移る合図)。
    fn next_candidate(&mut self) -> bool {
        if self.cand_index + 1 < self.cfg.inline_candidates {
            if self.cand_index + 1 >= self.candidates.len() {
                return true;
            }
            self.cand_index += 1;
            return false;
        }
        if !self.list_visible() {
            // 一覧の表示を始める
            if self.candidates.len() > self.cfg.inline_candidates {
                self.cand_index = self.cfg.inline_candidates;
                return false;
            }
            return true;
        }
        // 一覧が出ているときは頁単位で送る
        let (_, end) = self.page_range();
        if end < self.candidates.len() {
            self.cand_index = end;
            return false;
        }
        true
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
        self.cand_index = start
            .saturating_sub(self.cfg.page_size())
            .max(self.cfg.inline_candidates);
        if start == self.cfg.inline_candidates {
            self.cand_index = self.cfg.inline_candidates - 1;
        }
    }

    /// いま選んでいる候補を利用者辞書から取り除く。
    ///
    /// 候補が尽きたら ▽ に戻す。共有辞書には触れないので、そちら由来の候補は
    /// 次の変換でまた出る — 消えるのは学習による先頭への繰り上がりだけ。
    fn purge_candidate(&mut self) {
        let Some(c) = self.current_candidate() else {
            return;
        };
        let (key, text) = (c.key.clone(), c.cand.text.clone());
        self.dict.purge(&key, &text);
        self.candidates.retain(|c| c.cand.text != text);
        if self.candidates.is_empty() {
            self.phase = Phase::Composing;
            self.cand_index = 0;
        } else if self.cand_index >= self.candidates.len() {
            self.cand_index = self.candidates.len() - 1;
        }
    }

    fn commit_candidate(&mut self) -> String {
        let text = match self.current_candidate() {
            Some(c) => {
                let (key, cand) = (c.key.clone(), c.cand.clone());
                let shown = num::expand(&cand.text, &self.numbers);
                // 辞書へ書き戻すのは `#` のままの形。数字を戻した形で覚えると
                // その数字専用の項目になってしまう。
                self.dict.learn(&key, &cand);
                let okuri = self.okuri_head.is_some();
                self.note_commit(self.dict_key.clone(), shown.clone(), okuri);
                format!("{}{}", shown, self.okuri_kana)
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
        Skk::new(dict, Config::default())
    }

    fn typed(skk: &mut Skk, s: &str) -> String {
        let mut out = Vec::new();
        for c in s.chars() {
            let key = match c {
                '\n' => Key::Ctrl(0x0a),
                '\r' => Key::Enter,
                '\x07' => Key::Ctrl(0x07),
                '\x7f' => Key::Backspace,
                c => Key::Char(c),
            };
            out.extend(skk.handle(key).to_child);
        }
        String::from_utf8(out).unwrap()
    }

    /// 打ち込み中の内容だけ。モードの印は編集の対象ではないので外す。
    fn preedit_text(skk: &Skk) -> String {
        let p = skk.preedit();
        p.at_cursor
            .into_iter()
            .chain(p.floating)
            .filter(|s| {
                !matches!(
                    s.style,
                    Style::ModeHiragana
                        | Style::ModeKatakana
                        | Style::ModeHankaku
                        | Style::ModeZenkaku
                )
            })
            .map(|s| s.text)
            .collect()
    }

    /// モードの印だけ。
    fn mode_marker(skk: &Skk) -> String {
        skk.preedit()
            .at_cursor
            .into_iter()
            .filter(|s| {
                matches!(
                    s.style,
                    Style::ModeHiragana
                        | Style::ModeKatakana
                        | Style::ModeHankaku
                        | Style::ModeZenkaku
                )
            })
            .map(|s| s.text)
            .collect()
    }

    #[test]
    fn unknown_word_starts_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        // 候補が無いので登録に入る。見出しが出て、入力は直接入力に戻る。
        assert_eq!(preedit_text(&skk), "[登録:かんじ]");

        // 登録内容を打つ。子へは何も出ない。
        assert_eq!(typed(&mut skk, "kanji"), "");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]かんじ");

        // Enter で確定。ここで初めて子へ出る。
        assert_eq!(typed(&mut skk, "\r"), "かんじ");
        assert!(preedit_text(&skk).is_empty());

        // 覚えたので次からは変換できる
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼かんじ");
    }

    #[test]
    fn registration_keeps_conversion_available_inside() {
        let mut skk = skk_with(&[("かん", "/漢/"), ("じ", "/字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        // 登録の中でも変換できる
        typed(&mut skk, "Kan \nJi \n");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]漢字");
        assert_eq!(typed(&mut skk, "\r"), "漢字");
    }

    #[test]
    fn registration_with_okuri_registers_only_the_stem() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "UgoKu");
        // 送りありは見出しに送り仮名が添う
        assert_eq!(preedit_text(&skk), "[登録:うご*く]");
        // l で ASCII にして漢字を直接打ち込む
        typed(&mut skk, "l動");
        assert_eq!(preedit_text(&skk), "[登録:うご*く]動");
        // 登録されるのは語幹だけ。子へ出るのは送り仮名の付いた形。
        assert_eq!(typed(&mut skk, "\r"), "動く");
        typed(&mut skk, "\nUgoKu");
        assert_eq!(preedit_text(&skk), "▼動く");
    }

    #[test]
    fn cancelling_registration_returns_to_the_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        typed(&mut skk, "aiu");
        // C-g で登録を取り消すと ▽ に戻り、見出し語は残る
        assert_eq!(typed(&mut skk, "\x07"), "");
        assert_eq!(preedit_text(&skk), "▽かんじ");
    }

    #[test]
    fn empty_registration_returns_to_the_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(typed(&mut skk, "\r"), "");
        assert_eq!(preedit_text(&skk), "▽かんじ");
    }

    #[test]
    fn backspace_walks_out_of_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        typed(&mut skk, "ai");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]あい");
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]あ");
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]");
        // 空のところでもう一度押すと登録から抜ける
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "▽かんじ");
    }

    #[test]
    fn exhausting_the_candidates_starts_registration() {
        let mut skk = skk_with(&[("あい", "/愛/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Ai ");
        assert_eq!(preedit_text(&skk), "▼愛");
        // 候補を出し切ったところで space を押すと登録へ
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "[登録:あい]");
    }

    #[test]
    fn registration_nests() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        // 登録の中でさらに未知語を変換すると、もう一段積まれる
        typed(&mut skk, "Ai ");
        assert_eq!(preedit_text(&skk), "[[登録:あい]]");
        typed(&mut skk, "l愛");
        assert_eq!(typed(&mut skk, "\r"), "");
        // 内側が確定すると外側の登録内容になる
        assert_eq!(preedit_text(&skk), "[登録:かんじ]愛");
        assert_eq!(typed(&mut skk, "\r"), "愛");
    }

    #[test]
    fn control_keys_do_not_leak_into_the_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        typed(&mut skk, "ai");
        // 割り当ての無い C-z や矢印は登録内容に混ざらない
        assert_eq!(skk.handle(Key::Ctrl(0x1a)).to_child, Vec::<u8>::new());
        assert_eq!(
            skk.handle(Key::Raw(b"\x1b[A".to_vec())).to_child,
            Vec::<u8>::new()
        );
        assert_eq!(preedit_text(&skk), "[登録:かんじ]あい");
    }

    #[test]
    fn escape_returns_to_ascii() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "ai");
        // Esc は子へ渡りつつ、モードは ASCII に戻る (vim の挿入モードを抜ける動作)
        let r = skk.handle(Key::Esc);
        assert_eq!(r.to_child, vec![0x1b]);
        assert!(r.mode_changed);
        assert_eq!(skk.mode, Mode::Ascii);
        // 以降はそのまま英字が通る
        assert_eq!(typed(&mut skk, "dd"), "dd");
    }

    #[test]
    fn escape_confirms_the_reading_first() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼漢字");
        // 変換中に Esc を押すと、候補を確定してから抜ける
        let r = skk.handle(Key::Esc);
        assert_eq!(String::from_utf8(r.to_child).unwrap(), "漢字\u{1b}");
        assert_eq!(skk.mode, Mode::Ascii);
    }

    #[test]
    fn escape_does_not_disturb_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        typed(&mut skk, "ai");
        skk.handle(Key::Esc);
        // 登録中は打ち込みが消えないよう、モードも内容もそのまま
        assert_eq!(skk.mode, Mode::Hiragana);
        assert_eq!(preedit_text(&skk), "[登録:かんじ]あい");
    }

    #[test]
    fn ctrl_c_also_returns_to_ascii() {
        // nvim では C-c も挿入モードを抜ける (C-d は抜けないので既定に入れない)
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        let r = skk.handle(Key::Ctrl(0x03));
        assert_eq!(r.to_child, vec![0x03]);
        assert_eq!(skk.mode, Mode::Ascii);

        skk.handle(Key::Ctrl(0x0a));
        let r = skk.handle(Key::Ctrl(0x04));
        assert_eq!(r.to_child, vec![0x04]);
        assert_eq!(skk.mode, Mode::Hiragana, "C-d では抜けない");
    }

    #[test]
    fn ascii_keys_can_be_turned_off() {
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[behavior]\nascii_keys = []\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        let r = skk.handle(Key::Esc);
        assert_eq!(r.to_child, vec![0x1b]);
        assert_eq!(skk.mode, Mode::Hiragana);
    }

    #[test]
    fn tab_completes_the_reading() {
        let mut skk = skk_with(&[
            ("かんじ", "/漢字/"),
            ("かんじゃ", "/患者/"),
            ("かんきょう", "/環境/"),
            ("かい", "/回/"),
        ]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kan");
        // 「n」はまだローマ字のまま。補完はこれを「ん」に確定してから探す。
        assert_eq!(preedit_text(&skk), "▽かn");

        // 短い順、同じ長さなら辞書順
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじ");
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじゃ");
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんきょう");
        // 一周する
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじ");
        // Shift+Tab で戻る
        skk.handle(Key::Raw(b"\x1b[Z".to_vec()));
        assert_eq!(preedit_text(&skk), "▽かんきょう");

        // 補完したものはそのまま変換できる
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼環境");
    }

    #[test]
    fn cancel_undoes_the_completion() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kan");
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじ");
        // 一度目の C-g は補完を取り消すだけ。▽ は残る。
        typed(&mut skk, "\x07");
        assert_eq!(preedit_text(&skk), "▽かん");
        // 二度目で ▽ ごと取り消す
        typed(&mut skk, "\x07");
        assert!(preedit_text(&skk).is_empty());
    }

    #[test]
    fn typing_after_a_completion_starts_over() {
        let mut skk = skk_with(&[("かんじ", "/漢字/"), ("かんじゃ", "/患者/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kan");
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじ");
        // 打ち足すと並びは作り直される
        typed(&mut skk, "ya");
        assert_eq!(preedit_text(&skk), "▽かんじや");
        skk.handle(Key::Tab);
        assert_eq!(
            preedit_text(&skk),
            "▽かんじや",
            "前方一致しないので変わらない"
        );
    }

    #[test]
    fn tab_outside_conversion_reaches_the_child() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        // ASCII でもかなでも、直接入力中の TAB は子へ渡す (シェルの補完を殺さない)
        assert_eq!(skk.handle(Key::Tab).to_child, vec![0x09]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(skk.handle(Key::Tab).to_child, vec![0x09]);
    }

    #[test]
    fn purge_removes_the_learned_candidate() {
        let mut skk = skk_with(&[("かんじ", "/漢字/幹事/")]);
        skk.handle(Key::Ctrl(0x0a));
        // 一度「幹事」を確定して学習させる
        typed(&mut skk, "Kanji  \n");
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼幹事", "学習で先頭に来ている");

        // X で取り除くと次の候補に移る
        typed(&mut skk, "X");
        assert_eq!(preedit_text(&skk), "▼漢字");
        // 学習による繰り上がりが消え、共有辞書の並びに戻る
        typed(&mut skk, "\n");
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼漢字");
    }

    #[test]
    fn purging_the_last_candidate_returns_to_the_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        // 登録した語をひとつだけ持つ状態にする
        typed(&mut skk, "Tegaki ");
        typed(&mut skk, "lXY\r");
        typed(&mut skk, "\nTegaki ");
        assert_eq!(preedit_text(&skk), "▼XY");
        // 唯一の候補を消すと ▽ に戻る
        typed(&mut skk, "X");
        assert_eq!(preedit_text(&skk), "▽てがき");
    }

    #[test]
    fn numeric_conversion() {
        let mut skk = skk_with(&[("だい#かい", "/第#1回/第#0回/第#3回/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Dai5kai ");
        assert_eq!(preedit_text(&skk), "▼第５回");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼第5回");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼第五回");
        assert_eq!(typed(&mut skk, "\n"), "第五回");

        // 別の数字でも同じ項目が効く
        typed(&mut skk, "Dai12kai ");
        assert_eq!(preedit_text(&skk), "▼第十二回", "学習した #3 が先頭に来る");
        assert_eq!(typed(&mut skk, "\n"), "第十二回");
    }

    #[test]
    fn numeric_learning_keeps_the_hash_form() {
        let mut skk = skk_with(&[("だい#かい", "/第#1回/第#3回/")]);
        skk.handle(Key::Ctrl(0x0a));
        // 二番目 (#3) を選んで確定する
        typed(&mut skk, "Dai5kai  \n");
        // 辞書に書き戻されたのは `#` のままの形。数字専用の項目にはならない。
        let cands = skk.dict_mut().lookup("だい#かい");
        assert_eq!(cands[0].text, "第#3回");
        assert!(skk.dict_mut().lookup("だい5かい").is_empty());
    }

    #[test]
    fn literal_entry_wins_over_the_hash_form() {
        let mut skk = skk_with(&[
            ("だい#かい", "/第#1回/"),
            ("だい5かい", "/第五回だけの項目/"),
        ]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Dai5kai ");
        // 打った通りの見出し語が先
        assert_eq!(preedit_text(&skk), "▼第五回だけの項目");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼第５回");
    }

    #[test]
    fn a_reading_without_digits_is_untouched() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼漢字");
        assert_eq!(typed(&mut skk, "\n"), "漢字");
    }

    #[test]
    fn prefix_conversion() {
        let mut skk = skk_with(&[("あか>", "/赤/"), ("ぺん", "/ペン/")]);
        skk.handle(Key::Ctrl(0x0a));
        // ▽ の途中で > を押すと末尾に付いてすぐ変換が始まる
        typed(&mut skk, "Aka>");
        assert_eq!(preedit_text(&skk), "▼赤");
        assert_eq!(typed(&mut skk, "\n"), "赤");
        assert_eq!(typed(&mut skk, "Pen \n"), "ペン");
    }

    #[test]
    fn suffix_conversion() {
        let mut skk = skk_with(&[("かんどう", "/感動/"), (">てき", "/的/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kandou ");
        assert_eq!(preedit_text(&skk), "▼感動");
        // ▼ で > を押すと候補を確定し、> から始まる新しい見出し語を立てる
        assert_eq!(typed(&mut skk, ">"), "感動");
        assert_eq!(preedit_text(&skk), "▽>");
        typed(&mut skk, "teki");
        assert_eq!(preedit_text(&skk), "▽>てき");
        assert_eq!(typed(&mut skk, " \n"), "的");
    }

    #[test]
    fn learns_the_combined_word() {
        let mut skk = skk_with(&[("さい>", "/再/"), ("りよう", "/利用/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Sai>\n");
        typed(&mut skk, "Riyou \n");
        // 接頭辞に続いた語を繋げて覚える (ddskk の skk-learn-combined-word)
        let c = skk.dict_mut().lookup("さいりよう");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "再利用");
    }

    #[test]
    fn learns_the_combined_word_with_a_suffix() {
        let mut skk = skk_with(&[("かんどう", "/感動/"), (">てき", "/的/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kandou >teki \n");
        let c = skk.dict_mut().lookup("かんどうてき");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "感動的");
    }

    #[test]
    fn does_not_combine_across_other_input() {
        let mut skk = skk_with(&[("さい>", "/再/"), ("りよう", "/利用/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Sai>\n");
        // 間にかなを打ったら隣り合っていない
        typed(&mut skk, "no");
        typed(&mut skk, "Riyou \n");
        assert!(skk.dict_mut().lookup("さいりよう").is_empty());
    }

    #[test]
    fn combining_can_be_turned_off() {
        let mut skk = skk_with(&[("さい>", "/再/"), ("りよう", "/利用/")]);
        skk.set_config(Config::parse("[behavior]\nlearn_combined = false\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Sai>\nRiyou \n");
        assert!(skk.dict_mut().lookup("さいりよう").is_empty());
    }

    #[test]
    fn affix_marker_is_stripped_from_the_candidate() {
        // SKK-JISYO.L には候補側にも > が付く項目が 1 件ある
        let mut skk = skk_with(&[("さい>", "/再>/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Sai>");
        assert_eq!(preedit_text(&skk), "▼再");
    }

    #[test]
    fn a_bare_angle_bracket_passes_through() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        // 変換していないときの > はただの文字
        assert_eq!(typed(&mut skk, ">"), ">");
    }

    #[test]
    fn hankaku_katakana_mode() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        // C-q で半角カタカナモードへ
        assert!(skk.handle(Key::Ctrl(0x11)).mode_changed);
        assert_eq!(skk.mode, Mode::HankakuKatakana);
        assert_eq!(typed(&mut skk, "nihongo"), "ﾆﾎﾝｺﾞ");
        // もう一度でひらがなへ戻る
        skk.handle(Key::Ctrl(0x11));
        assert_eq!(skk.mode, Mode::Hiragana);
        assert_eq!(typed(&mut skk, "aiu"), "あいう");
    }

    #[test]
    fn hankaku_katakana_from_the_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Nihongo");
        assert_eq!(preedit_text(&skk), "▽にほんご");
        // ▽ の途中の C-q は見出し語を半角カタカナにして確定する
        let r = skk.handle(Key::Ctrl(0x11));
        assert_eq!(String::from_utf8(r.to_child).unwrap(), "ﾆﾎﾝｺﾞ");
        assert!(preedit_text(&skk).is_empty());
    }

    #[test]
    fn hankaku_katakana_shows_in_the_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        skk.handle(Key::Ctrl(0x11));
        typed(&mut skk, "Nihongo");
        // 見出し語もモードに合わせて出る
        assert_eq!(preedit_text(&skk), "▽ﾆﾎﾝｺﾞ");
    }

    #[test]
    fn float_layout_moves_the_list_off_the_line() {
        let mut skk = skk_with(&[("あい", "/愛/藍/相/合/挨/曖/哀/")]);
        skk.set_config(Config::parse("[candidates]\nlayout = \"float\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Ai     ");
        let p = skk.preedit();
        // 行内は ▼ とモードの印だけ。一覧は浮かせる側へ。
        assert_eq!(
            p.at_cursor
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "▼挨"
        );
        assert!(p.floating.iter().any(|s| s.text.contains("挨")));
        assert!(!p.floating.is_empty());
    }

    #[test]
    fn inline_layout_keeps_everything_on_the_line() {
        let mut skk = skk_with(&[("あい", "/愛/藍/相/合/挨/曖/哀/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Ai     ");
        let p = skk.preedit();
        assert!(p.floating.is_empty());
        assert!(p.at_cursor.iter().any(|s| s.text.contains("挨")));
    }

    #[test]
    fn shows_how_many_candidates_remain() {
        // 一覧に載りきらない分は件数で知らせる (ddskk と同じ)
        let mut skk = skk_with(&[("あい", "/1/2/3/4/5/6/7/8/9/10/11/12/13/14/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Ai     ");
        assert!(preedit_text(&skk).contains("[残り 3]"));
    }

    #[test]
    fn cell_marker_tints_the_cursor_without_adding_anything() {
        let mut skk = skk_with(&[]);
        // 既定はセルに色を敷く方式。文字は足さない。
        assert_eq!(skk.marker(), Marker::Cell);
        assert!(skk.preedit().cursor_tint.is_none(), "ASCII では何もしない");
        assert!(skk.preedit().is_empty());

        skk.handle(Key::Ctrl(0x0a));
        let p = skk.preedit();
        assert_eq!(
            p.cursor_tint,
            Some(Tint {
                style: Style::ModeHiragana,
                offset: 0,
                glyph: None
            })
        );
        assert!(p.at_cursor.is_empty(), "文字は足さない");
        assert!(!p.is_empty(), "色を敷くので描くものはある");

        skk.handle(Key::Char('q'));
        assert_eq!(
            skk.preedit().cursor_tint.map(|t| t.style),
            Some(Style::ModeKatakana)
        );

        // 打ち込み中はその先頭が同じ場所に来るので敷かない
        skk.handle(Key::Char('q'));
        typed(&mut skk, "Kanji");
        assert!(skk.preedit().cursor_tint.is_none());
        assert_eq!(preedit_text(&skk), "▽かんじ");
    }

    #[test]
    fn beside_marker_sits_next_to_the_cursor() {
        // カーソルに覆われない位置に置く。多重化器がカーソルの見た目を遅れて
        // 同期する環境でも確実に見える。
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[behavior]\nmode_marker = \"beside\"\n").unwrap());
        assert!(skk.preedit().cursor_tint.is_none(), "ASCII では何もしない");
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(
            skk.preedit().cursor_tint,
            Some(Tint {
                style: Style::ModeHiragana,
                offset: 1,
                glyph: None
            })
        );
        assert!(skk.preedit().at_cursor.is_empty(), "文字は足さない");
    }

    #[test]
    fn symbol_marker_shows_a_halfwidth_letter() {
        // 色に頼らずモードが分かる。幅は 1 桁なので見た目も崩れない。
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[behavior]\nmode_marker = \"symbol\"\n").unwrap());
        assert!(skk.preedit().cursor_tint.is_none(), "ASCII では何もしない");
        for (key, glyph) in [
            (Key::Ctrl(0x0a), '~'),
            (Key::Char('q'), '+'),
            (Key::Ctrl(0x11), '-'),
        ] {
            skk.handle(key);
            let t = skk.preedit().cursor_tint.expect("印が出る");
            assert_eq!(t.glyph, Some(glyph));
            assert_eq!(t.offset, 0, "カーソルの真上");
        }
        skk.handle(Key::Ctrl(0x11));
        skk.handle(Key::Char('L'));
        assert_eq!(skk.preedit().cursor_tint.unwrap().glyph, Some('@'));

        // 記号は設定で変えられる。半角一桁だけを認める。
        skk.set_config(
            Config::parse(
                "[behavior]\nmode_marker = \"symbol\"\n[behavior.mode_symbols]\nhiragana = \"#\"\n",
            )
            .unwrap(),
        );
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(skk.preedit().cursor_tint.unwrap().glyph, Some('#'));
        assert!(Config::parse("[behavior.mode_symbols]\nhiragana = \"あ\"\n").is_err());
        assert!(Config::parse("[behavior.mode_symbols]\nhiragana = \"ab\"\n").is_err());
        assert!(Config::parse("[behavior.mode_symbols]\nfoo = \"#\"\n").is_err());
    }

    #[test]
    fn letter_marker_appends_a_letter() {
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[behavior]\nmode_marker = \"letter\"\n").unwrap());
        assert_eq!(mode_marker(&skk), "", "ASCII では何も描かない");

        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(mode_marker(&skk), "あ");
        skk.handle(Key::Char('q'));
        assert_eq!(mode_marker(&skk), "ア");
        skk.handle(Key::Ctrl(0x11));
        assert_eq!(mode_marker(&skk), "半");
        skk.handle(Key::Ctrl(0x11));
        skk.handle(Key::Char('L'));
        assert_eq!(mode_marker(&skk), "Ａ");

        // 印は打ち込み中の内容より後ろに出る
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        let texts: Vec<String> = skk
            .preedit()
            .at_cursor
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(texts.last().map(|s| s.as_str()), Some("あ"));
        assert!(skk.preedit().cursor_tint.is_none());
    }

    #[test]
    fn mode_marker_can_be_turned_off() {
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[behavior]\nmode_marker = \"off\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(mode_marker(&skk), "");
        assert!(skk.preedit().cursor_tint.is_none());
        assert!(
            skk.preedit().is_empty(),
            "何も打っていなければ描くものが無い"
        );
    }

    #[test]
    fn custom_bindings_are_honoured() {
        let cfg = Config::parse(
            r#"
            [keys]
            kana = "C-o"
            ascii = "@"
            katakana = "~"
            convert = "C-space"
            previous = "-"
            select = ["1", "2"]

            [candidates]
            inline = 1
            "#,
        )
        .unwrap();
        let mut skk = skk_with(&[("あい", "/愛/藍/相/合/"), ("かんじ", "/漢字/")]);
        skk.set_config(cfg);

        // C-j はもう効かない (素のまま子へ行く)
        assert_eq!(skk.handle(Key::Ctrl(0x0a)).to_child, vec![0x0a]);
        // C-o でかなモードへ
        assert!(skk.handle(Key::Ctrl(0x0f)).mode_changed);
        assert_eq!(typed(&mut skk, "ai"), "あい");
        // ~ でカタカナ、@ で ASCII
        skk.handle(Key::Char('~'));
        assert_eq!(skk.mode, Mode::Katakana);
        skk.handle(Key::Char('@'));
        assert_eq!(skk.mode, Mode::Ascii);

        // 変換は C-space、一覧は inline = 1 なので 2 番目から出て 1 2 で選ぶ
        skk.handle(Key::Ctrl(0x0f));
        typed(&mut skk, "Ai");
        skk.handle(Key::Ctrl(0x00));
        assert_eq!(preedit_text(&skk), "▼愛");
        skk.handle(Key::Ctrl(0x00));
        assert!(preedit_text(&skk).contains("1:藍"));
        assert!(preedit_text(&skk).contains("2:相"));
        assert_eq!(skk.handle(Key::Char('2')).to_child, "相".as_bytes());
    }

    #[test]
    fn reconfiguring_mid_conversion_keeps_the_state() {
        let mut skk = skk_with(&[("かんじ", "/漢字/感じ/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼漢字");
        // 変換の途中で設定が差し替わっても見出し語と候補は残る
        let cfg = Config::parse("[keys]\nconvert = \"C-n\"\n").unwrap();
        skk.set_config(cfg);
        assert_eq!(preedit_text(&skk), "▼漢字");
        skk.handle(Key::Ctrl(0x0e));
        assert_eq!(preedit_text(&skk), "▼感じ");
        // 古い割り当ての space は候補を確定してから子へ流れる
        assert_eq!(typed(&mut skk, " "), "感じ ");
    }

    #[test]
    fn enter_confirms_regardless_of_bindings() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.set_config(Config::parse("[keys]\nconfirm = \"C-m\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        // 変換中の Enter は改行を送らずに確定する
        let r = skk.handle(Key::Enter);
        assert_eq!(String::from_utf8(r.to_child).unwrap(), "漢字");
        // 直接入力に戻れば Enter は素通し (コマンドの実行を邪魔しない)
        assert_eq!(skk.handle(Key::Enter).to_child, vec![0x0d]);
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
