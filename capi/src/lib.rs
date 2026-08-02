//! ttyskk の変換エンジンを C から使うための層。
//!
//! GUI の入力メソッド (fcitx5 の addon など) から呼ぶことを想定している。設計は
//! `docs/fcitx5-addon.md` を参照。
//!
//! # 約束
//!
//! - **返した文字列は、次に同じハンドルへ何かを呼ぶまで有効**。呼ぶ側で解放しない。
//!   確保と解放を往復させるより単純で、入力メソッドは同期的に使うので困らない。
//! - ハンドルは [`ttyskk_new`] で作り [`ttyskk_free`] で捨てる。**スレッドを跨いで
//!   同時に触ってはいけない** (入力メソッドの本体は単一スレッドで回る)。
//! - どの関数も panic を外へ漏らさない。漏らすと C 側から見て未定義の動きになるため、
//!   境界で受け止めて無難な値を返す。
//!
//! # キーの畳み込み
//!
//! `keysym` と `modifiers` は X11 の値をそのまま受ける (fcitx5 が持っている形)。
//! これを [`ttyskk::skk::Key`] に直すのはこの層の仕事で、エンジンには持ち込まない。
//! 端末側の `input.rs` がバイト列から `Key` を作るのと同じ立場にある。

use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use ttyskk::config::Config;
use ttyskk::dict::Dict;
use ttyskk::skk::{Key, Mode, Skk, Style};

/// 変換エンジンひとつ分。C 側からは不透明な入れ物として扱う。
pub struct TtyskkEngine {
    skk: Skk,
    /// 直前の打鍵で確定した文字列。返したポインタを次の呼び出しまで有効に保つ。
    commit: CString,
    /// 入力中の表示 (モードの印は除いてある)。
    preedit: Vec<(CString, i32)>,
    /// 選択中の候補。▼ でなければ空。
    candidates: Vec<(CString, CString)>,
    selected: usize,
    /// 一覧を出す段階に来ているか。
    list_visible: bool,
    labels: CString,
}

/// 入力モード。`ttyskk_mode` が返す。
pub const TTYSKK_MODE_ASCII: i32 = 0;
pub const TTYSKK_MODE_HIRAGANA: i32 = 1;
pub const TTYSKK_MODE_KATAKANA: i32 = 2;
pub const TTYSKK_MODE_HANKAKU_KATAKANA: i32 = 3;
pub const TTYSKK_MODE_ZENKAKU_ASCII: i32 = 4;

/// 入力中の表示の装飾。`ttyskk_preedit_style` が返す。
pub const TTYSKK_STYLE_READING: i32 = 0;
pub const TTYSKK_STYLE_ROMAJI: i32 = 1;
pub const TTYSKK_STYLE_CANDIDATE: i32 = 2;
/// 見出し語の中でカーソルが乗っている一文字。入力メソッド側で位置を示すのに使う。
pub const TTYSKK_STYLE_READING_CURSOR: i32 = 3;
/// 打つそばから見せている補完。**まだ打っていない文字**なので、打った分と
/// 見分けの付く見た目にする (端末では薄字)。
pub const TTYSKK_STYLE_COMPLETION: i32 = 4;

fn mode_code(m: Mode) -> i32 {
    match m {
        Mode::Ascii => TTYSKK_MODE_ASCII,
        Mode::Hiragana => TTYSKK_MODE_HIRAGANA,
        Mode::Katakana => TTYSKK_MODE_KATAKANA,
        Mode::HankakuKatakana => TTYSKK_MODE_HANKAKU_KATAKANA,
        Mode::ZenkakuAscii => TTYSKK_MODE_ZENKAKU_ASCII,
    }
}

/// 表示の装飾。GUI に渡さないものは `None`。
///
/// モードの印は端末でカーソルに色を敷くためのもので、GUI では入力メソッドの
/// インジケータが担う。候補一覧は候補窓へ回すので、入力中の表示には混ぜない。
fn style_code(s: Style) -> Option<i32> {
    match s {
        Style::Reading => Some(TTYSKK_STYLE_READING),
        Style::ReadingCursor => Some(TTYSKK_STYLE_READING_CURSOR),
        Style::Romaji => Some(TTYSKK_STYLE_ROMAJI),
        Style::Candidate => Some(TTYSKK_STYLE_CANDIDATE),
        // 補完は入力中の表示に混ぜる。打っている場所の続きとして見えないと読めない。
        Style::Completion => Some(TTYSKK_STYLE_COMPLETION),
        Style::ListItem | Style::ListSelected => None,
        Style::ModeHiragana | Style::ModeKatakana | Style::ModeHankaku | Style::ModeZenkaku => None,
    }
}

/// 修飾キーそのものの押し下げか。
///
/// fcitx5 は Shift や Ctrl を押した時点でも打鍵として渡してくる。端末ではバイトが
/// 流れないので起こらない話で、GUI 側だけの事情。**これを「解釈しないキー」として
/// 扱ってはいけない** — 手前を確定してしまい、▽おく の続きに送り仮名を打とうとして
/// Shift を押し下げた瞬間に「おく」が出てしまう。押しただけでは何も起こさない。
fn is_modifier(keysym: u32) -> bool {
    matches!(
        keysym,
        // ISO_Lock … ISO_Level5_Lock (第3・第5水準の Shift/Latch/Lock)
        0xfe01..=0xfe13
        // Mode_switch (ISO_Group_Shift) と Num_Lock
        | 0xff7e | 0xff7f
        // Shift / Control / Caps_Lock / Meta / Alt / Super / Hyper
        | 0xffe1..=0xffee
    )
}

/// X11 の keysym と修飾キーを [`Key`] に畳む。
///
/// 畳めないもの (矢印・機能キー) は `None` を返し、呼ぶ側の入力メソッドへ委ねる。
/// エンジンが解釈しないキーをわざわざ渡す必要は無い。
fn to_key(keysym: u32, modifiers: u32) -> Option<Key> {
    // X11 の ModifierMask。fcitx5 の KeyState も同じ並び。
    const SHIFT: u32 = 1 << 0;
    const CTRL: u32 = 1 << 2;
    // 一文字も伴わない修飾だけの打鍵は、押した時点では何も起こさない
    const ALT: u32 = 1 << 3;
    const SUPER: u32 = 1 << 6;

    if modifiers & (ALT | SUPER) != 0 {
        return None;
    }
    let ctrl = modifiers & CTRL != 0;
    let shift = modifiers & SHIFT != 0;

    match keysym {
        // Return / KP_Enter
        0xff0d | 0xff8d => Some(Key::Enter),
        0xff08 => Some(Key::Backspace),
        0xff09 if !shift && !ctrl => Some(Key::Tab),
        // ISO_Left_Tab。端末の CSI Z にあたる
        0xfe20 => Some(Key::ShiftTab),
        0xff1b => Some(Key::Esc),
        // 印字できる ASCII。keysym は文字コードそのもの
        0x20..=0x7e => {
            let c = char::from_u32(keysym)?;
            if ctrl {
                // C-a = 0x01 … C-z = 0x1a、C-space = 0x00
                match c {
                    ' ' => Some(Key::Ctrl(0x00)),
                    'a'..='z' => Some(Key::Ctrl(c as u8 & 0x1f)),
                    'A'..='Z' => Some(Key::Ctrl(c.to_ascii_lowercase() as u8 & 0x1f)),
                    _ => None,
                }
            } else {
                Some(Key::Char(c))
            }
        }
        // Unicode keysym (0x01000000 | コードポイント)。直接かなを送ってくる環境向け
        0x01000000..=0x0110ffff if !ctrl => char::from_u32(keysym - 0x01000000).map(Key::Char),
        _ => None,
    }
}

/// C の文字列を借りる。NULL と壊れた UTF-8 は `None`。
///
/// # Safety
/// `p` は NULL か、NUL で終わる有効な文字列を指していること。
unsafe fn borrow<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

fn cstring(s: &str) -> CString {
    // 内側の NUL は落とす。C の文字列にできない値を渡されても壊れないように。
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

impl TtyskkEngine {
    /// 直前の打鍵の結果を、C へ返せる形に写し取る。
    fn refresh(&mut self, commit: &str) {
        self.commit = cstring(commit);

        self.preedit.clear();
        let p = self.skk.preedit();
        for seg in p.at_cursor.into_iter().chain(p.floating) {
            if let Some(style) = style_code(seg.style) {
                self.preedit.push((cstring(&seg.text), style));
            }
        }

        self.candidates.clear();
        self.selected = 0;
        self.list_visible = false;
        self.labels = CString::default();
        if let Some(v) = self.skk.candidates() {
            // SKK では最初の数件を一つずつ送り、それを過ぎたら一覧に切り替える
            self.list_visible = v.selected >= v.inline_until;
            for item in &v.items {
                let annot = item.annotation.clone().unwrap_or_default();
                self.candidates.push((cstring(&item.text), cstring(&annot)));
            }
            self.selected = v.selected;
            self.labels = cstring(&v.select_keys.iter().collect::<String>());
        }
    }
}

/// panic を C の境界で受け止める。
fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(fallback)
}

/// エンジンを作る。作れなければ NULL。
///
/// - `system_jisyo_paths` … 共有辞書のパスを `:` で繋いだもの。NULL なら共有辞書なし
/// - `user_jisyo_path` … 利用者辞書のパス。学習はここへ書き戻す
/// - `config_toml` … 設定ファイルの中身そのもの。NULL なら既定
///
/// # Safety
/// 引数はいずれも NULL か、NUL で終わる有効な文字列を指していること。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_new(
    system_jisyo_paths: *const c_char,
    user_jisyo_path: *const c_char,
    config_toml: *const c_char,
) -> *mut TtyskkEngine {
    guard(std::ptr::null_mut(), || {
        let system: Vec<PathBuf> = unsafe { borrow(system_jisyo_paths) }
            .unwrap_or_default()
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        let user = PathBuf::from(unsafe { borrow(user_jisyo_path) }.unwrap_or_default());
        // 読めない設定は捨てて既定で動く。端末側と同じ扱い。
        let cfg = unsafe { borrow(config_toml) }
            .and_then(|text| Config::parse(text).ok())
            .unwrap_or_default();
        let Ok(dict) = Dict::load(&system, user, None) else {
            return std::ptr::null_mut();
        };
        let mut engine = Box::new(TtyskkEngine {
            skk: Skk::new(dict, cfg),
            commit: CString::default(),
            preedit: Vec::new(),
            candidates: Vec::new(),
            selected: 0,
            list_visible: false,
            labels: CString::default(),
        });
        engine.refresh("");
        Box::into_raw(engine)
    })
}

/// エンジンを捨てる。学習は書き出さないので、必要なら先に [`ttyskk_save`]。
///
/// # Safety
/// `p` は [`ttyskk_new`] が返したもので、まだ捨てていないこと。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_free(p: *mut TtyskkEngine) {
    if p.is_null() {
        return;
    }
    guard((), || drop(unsafe { Box::from_raw(p) }));
}

/// 設定を入れ替える。読めなければ何もしない。
///
/// # Safety
/// `p` は有効なハンドル、`config_toml` は NULL か有効な文字列。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_set_config(p: *mut TtyskkEngine, config_toml: *const c_char) {
    let Some(e) = (unsafe { p.as_mut() }) else {
        return;
    };
    guard((), || {
        if let Some(text) = unsafe { borrow(config_toml) }
            && let Ok(cfg) = Config::parse(text)
        {
            e.skk.set_config(cfg);
        }
    });
}

/// 学習を利用者辞書へ書き出す。
///
/// ディスクの現状を読み直してから重ねるので、端末側の ttyskk が同時に動いていても
/// 互いの学習を消さない。
///
/// # Safety
/// `p` は有効なハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_save(p: *mut TtyskkEngine) {
    let Some(e) = (unsafe { p.as_mut() }) else {
        return;
    };
    guard((), || {
        let _ = e.skk.dict_mut().save();
    });
}

/// 入力中の内容をすべて捨てる。モードは変えない。
///
/// # Safety
/// `p` は有効なハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_reset(p: *mut TtyskkEngine) {
    let Some(e) = (unsafe { p.as_mut() }) else {
        return;
    };
    guard((), || {
        e.skk.clear();
        e.refresh("");
    });
}

/// 文脈を渡す意味があるか。
///
/// 設定 (`[behavior] context_order`) が無効なら false。**周辺テキストを組み立てる前に
/// これを見る** — 呼ぶ側で毎打鍵ごとに文字列を作る手間を省ける。
///
/// # Safety
/// `p` は有効なハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_wants_context(p: *const TtyskkEngine) -> bool {
    unsafe { p.as_ref() }.is_some_and(|e| e.skk.wants_context())
}

/// 入力欄に見えている文章を文脈として渡す。同音異義語の順序に効く。
///
/// - `text` … UTF-8 の文字列。NULL か空なら**文脈を忘れる** (順序を変えなくなる)
/// - `cursor` … カーソルの位置。**バイト数ではなく文字数**で数える。fcitx5 の
///   `SurroundingText::cursor()` はこの単位なのでそのまま渡してよい。長すぎる値は
///   末尾に丸める
///
/// 渡した文脈は次に渡し直すまで残る。**入力欄が周辺テキストを持たない場に移ったら、
/// 空を渡して忘れさせること** — 前の窓の話題が残ったまま並べ替えてしまう。
///
/// # Safety
/// `p` は有効なハンドル。`text` は NULL か、NUL で終わる有効な文字列。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_set_context(
    p: *mut TtyskkEngine,
    text: *const c_char,
    cursor: usize,
) {
    let Some(e) = (unsafe { p.as_mut() }) else {
        return;
    };
    guard((), || {
        // 壊れた UTF-8 は「文脈なし」として扱う。手掛かりが無いだけで実害は無い。
        let text = unsafe { borrow(text) }.unwrap_or_default();
        e.skk.set_context(text, cursor);
    });
}

/// キーを一つ渡す。**エンジンが受け取ったなら true**。
///
/// false のときは呼ぶ側がそのキーを自分で扱う (矢印や機能キー、ASCII モードの打鍵)。
/// **false でも確定した文字列が出ていることがある** — ▽ の途中で矢印を押すと、見出し語
/// を確定したうえで矢印は呼ぶ側へ渡る。[`ttyskk_commit`] は必ず見ること。
///
/// # Safety
/// `p` は有効なハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_key(p: *mut TtyskkEngine, keysym: u32, modifiers: u32) -> bool {
    let Some(e) = (unsafe { p.as_mut() }) else {
        return false;
    };
    guard(false, || {
        // 修飾キーそのものは、押し下げた時点では何も起こさない。手前の確定もしない。
        // (直前の確定を持ち越さないよう、返す文字列だけは空に戻しておく)
        if is_modifier(keysym) {
            e.refresh("");
            return false;
        }
        let Some(key) = to_key(keysym, modifiers) else {
            // 解釈しないキー (矢印・機能キー)。手前までを確定してから呼ぶ側へ委ねる。
            let commit = e.skk.flush();
            e.refresh(&commit);
            return false;
        };
        let r = e.skk.handle(key);
        let handled = r.passthrough.is_none();
        e.refresh(&r.commit);
        handled
    })
}

/// 直前の打鍵で確定した文字列。確定していなければ空文字列。
///
/// # Safety
/// `p` は有効なハンドル。返ったポインタは次に同じハンドルを触るまで有効。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_commit(p: *const TtyskkEngine) -> *const c_char {
    match unsafe { p.as_ref() } {
        Some(e) => e.commit.as_ptr(),
        None => c"".as_ptr(),
    }
}

/// いまの入力モード (`TTYSKK_MODE_*`)。
///
/// # Safety
/// `p` は有効なハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_mode(p: *const TtyskkEngine) -> i32 {
    match unsafe { p.as_ref() } {
        Some(e) => mode_code(e.skk.mode()),
        None => TTYSKK_MODE_ASCII,
    }
}

/// 入力中の表示の区間数。
///
/// # Safety
/// `p` は有効なハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_preedit_len(p: *const TtyskkEngine) -> usize {
    unsafe { p.as_ref() }.map_or(0, |e| e.preedit.len())
}

/// `i` 番目の区間の文字列。範囲外なら空文字列。
///
/// # Safety
/// `p` は有効なハンドル。返ったポインタは次に同じハンドルを触るまで有効。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_preedit_text(p: *const TtyskkEngine, i: usize) -> *const c_char {
    match unsafe { p.as_ref() }.and_then(|e| e.preedit.get(i)) {
        Some((s, _)) => s.as_ptr(),
        None => c"".as_ptr(),
    }
}

/// `i` 番目の区間の装飾 (`TTYSKK_STYLE_*`)。
///
/// # Safety
/// `p` は有効なハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_preedit_style(p: *const TtyskkEngine, i: usize) -> i32 {
    unsafe { p.as_ref() }
        .and_then(|e| e.preedit.get(i))
        .map_or(TTYSKK_STYLE_READING, |(_, st)| *st)
}

/// 候補の数。選択中 (▼) でなければ 0。
///
/// # Safety
/// `p` は有効なハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_candidate_len(p: *const TtyskkEngine) -> usize {
    unsafe { p.as_ref() }.map_or(0, |e| e.candidates.len())
}

/// `i` 番目の候補。範囲外なら空文字列。
///
/// # Safety
/// `p` は有効なハンドル。返ったポインタは次に同じハンドルを触るまで有効。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_candidate_text(p: *const TtyskkEngine, i: usize) -> *const c_char {
    match unsafe { p.as_ref() }.and_then(|e| e.candidates.get(i)) {
        Some((s, _)) => s.as_ptr(),
        None => c"".as_ptr(),
    }
}

/// `i` 番目の候補の注釈 (辞書の `;` 以降)。無ければ空文字列。
///
/// # Safety
/// `p` は有効なハンドル。返ったポインタは次に同じハンドルを触るまで有効。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_candidate_annotation(
    p: *const TtyskkEngine,
    i: usize,
) -> *const c_char {
    match unsafe { p.as_ref() }.and_then(|e| e.candidates.get(i)) {
        Some((_, a)) => a.as_ptr(),
        None => c"".as_ptr(),
    }
}

/// 選ばれている候補の位置。
///
/// # Safety
/// `p` は有効なハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_candidate_selected(p: *const TtyskkEngine) -> usize {
    unsafe { p.as_ref() }.map_or(0, |e| e.selected)
}

/// 候補の一覧を出す段階か。
///
/// SKK では最初の数件を一つずつ送り、それを過ぎたところで一覧に切り替える習わし
/// (何件目からかは設定の `candidates.inline`)。**候補があること**と**一覧を出すこと**
/// は別なので、窓を出すかどうかはこちらで判断する。
///
/// # Safety
/// `p` は有効なハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_candidate_visible(p: *const TtyskkEngine) -> bool {
    unsafe { p.as_ref() }.is_some_and(|e| e.list_visible)
}

/// 候補一覧から選ぶキーを並べたもの ("asdfjkl" など)。文字数が一頁の大きさになる。
///
/// # Safety
/// `p` は有効なハンドル。返ったポインタは次に同じハンドルを触るまで有効。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ttyskk_candidate_labels(p: *const TtyskkEngine) -> *const c_char {
    match unsafe { p.as_ref() } {
        Some(e) => e.labels.as_ptr(),
        None => c"".as_ptr(),
    }
}

#[cfg(test)]
mod tests;
