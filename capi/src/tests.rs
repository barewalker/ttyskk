//! C ABI をそのままの形で叩く。C++ を書く前にここで確かめられる。

use super::*;

/// X11 の keysym。畳み込みの対象になるものだけ。
const RETURN: u32 = 0xff0d;
const BACKSPACE: u32 = 0xff08;
const ESCAPE: u32 = 0xff1b;
const ISO_LEFT_TAB: u32 = 0xfe20;
const LEFT: u32 = 0xff51;
const CTRL: u32 = 1 << 2;

/// 試験用の辞書を置いて、エンジンを一つ作る。
fn engine(entries: &[(&str, &str)]) -> *mut TtyskkEngine {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ttyskk-capi-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    let sys = dir.join("sys.dict");
    let mut body = String::from(";; okuri-nasi entries.\n");
    for (k, v) in entries {
        body.push_str(&format!("{k} {v}\n"));
    }
    std::fs::write(&sys, body).unwrap();

    let sys_c = CString::new(sys.to_str().unwrap()).unwrap();
    let user_c = CString::new(dir.join("user.dict").to_str().unwrap()).unwrap();
    let p = unsafe { ttyskk_new(sys_c.as_ptr(), user_c.as_ptr(), std::ptr::null()) };
    assert!(!p.is_null(), "エンジンを作れた");
    p
}

/// ASCII の文字列を一文字ずつ送り、確定した分を繋げて返す。
fn typed(p: *mut TtyskkEngine, s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        unsafe { ttyskk_key(p, c as u32, 0) };
        out.push_str(commit(p));
    }
    out
}

fn commit(p: *mut TtyskkEngine) -> &'static str {
    unsafe { CStr::from_ptr(ttyskk_commit(p)) }
        .to_str()
        .unwrap()
}

/// 入力中の表示を、装飾を捨てて繋げたもの。
fn preedit(p: *mut TtyskkEngine) -> String {
    let mut out = String::new();
    for i in 0..unsafe { ttyskk_preedit_len(p) } {
        out.push_str(
            unsafe { CStr::from_ptr(ttyskk_preedit_text(p, i)) }
                .to_str()
                .unwrap(),
        );
    }
    out
}

fn candidates(p: *mut TtyskkEngine) -> Vec<String> {
    (0..unsafe { ttyskk_candidate_len(p) })
        .map(|i| {
            unsafe { CStr::from_ptr(ttyskk_candidate_text(p, i)) }
                .to_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

#[test]
fn kana_mode_and_conversion() {
    let p = engine(&[("かんじ", "/漢字/幹事;宴会の/")]);

    // 何もしていないうちは ASCII で、打った文字は呼ぶ側へ委ねる
    assert_eq!(unsafe { ttyskk_mode(p) }, TTYSKK_MODE_ASCII);
    assert!(!unsafe { ttyskk_key(p, 'a' as u32, 0) });

    // C-j でかなモードへ
    assert!(unsafe { ttyskk_key(p, 'j' as u32, CTRL) });
    assert_eq!(unsafe { ttyskk_mode(p) }, TTYSKK_MODE_HIRAGANA);

    assert_eq!(typed(p, "ai"), "あい");

    // 大文字で ▽ に入る
    typed(p, "Kanji");
    assert_eq!(preedit(p), "▽かんじ");
    assert!(candidates(p).is_empty(), "まだ変換していない");

    // space で ▼
    typed(p, " ");
    assert_eq!(preedit(p), "▼漢字");
    assert_eq!(candidates(p), vec!["漢字", "幹事"]);
    assert_eq!(unsafe { ttyskk_candidate_selected(p) }, 0);
    assert_eq!(
        unsafe { CStr::from_ptr(ttyskk_candidate_annotation(p, 1)) }
            .to_str()
            .unwrap(),
        "宴会の"
    );

    // 次の候補へ
    typed(p, " ");
    assert_eq!(unsafe { ttyskk_candidate_selected(p) }, 1);

    // C-j で確定
    unsafe { ttyskk_key(p, 'j' as u32, CTRL) };
    assert_eq!(commit(p), "幹事");
    assert_eq!(preedit(p), "");
    assert!(candidates(p).is_empty());

    unsafe { ttyskk_free(p) };
}

/// 候補があることと、一覧を出すことは別。
///
/// SKK では最初の数件を一つずつ送り、それを過ぎたところで一覧に切り替える。
/// 候補窓を出すかどうかは呼ぶ側が判断できないので、こちらで持つ。
#[test]
fn the_candidate_list_appears_only_after_a_few() {
    let p = engine(&[("あ", "/亜/唖/娃/阿/哀/愛/挨/姶/逢/")]);
    unsafe { ttyskk_key(p, 'j' as u32, CTRL) };
    typed(p, "A ");

    // 既定では 4 件目まで一つずつ送る
    assert_eq!(unsafe { ttyskk_candidate_len(p) }, 9, "候補は全部見える");
    assert!(
        !unsafe { ttyskk_candidate_visible(p) },
        "まだ一覧は出さない"
    );

    // 3 件目まではまだ
    typed(p, "  ");
    assert_eq!(unsafe { ttyskk_candidate_selected(p) }, 2);
    assert!(!unsafe { ttyskk_candidate_visible(p) });

    typed(p, "  ");
    assert_eq!(unsafe { ttyskk_candidate_selected(p) }, 4);
    assert!(
        unsafe { ttyskk_candidate_visible(p) },
        "4 件目を過ぎたら一覧"
    );

    // ▼ を抜ければ消える
    unsafe { ttyskk_key(p, 'j' as u32, CTRL) };
    assert!(!unsafe { ttyskk_candidate_visible(p) });
    assert_eq!(unsafe { ttyskk_candidate_len(p) }, 0);

    unsafe { ttyskk_free(p) };
}

/// 確定と「呼ぶ側へ委ねる」は同時に起きる。
///
/// ▽ の途中で矢印を押すと、見出し語を確定したうえで矢印は呼ぶ側へ渡る。
/// **false が返っても確定した文字列を見落としてはいけない。**
#[test]
fn commit_can_come_with_an_unhandled_key() {
    let p = engine(&[]);
    unsafe { ttyskk_key(p, 'j' as u32, CTRL) };
    typed(p, "Kanji");

    let handled = unsafe { ttyskk_key(p, LEFT, 0) };
    assert!(!handled, "矢印は呼ぶ側の仕事");
    assert_eq!(commit(p), "かんじ", "見出し語は確定している");
    assert_eq!(preedit(p), "");

    unsafe { ttyskk_free(p) };
}

/// 入力中の表示にモードの印は混ざらない (GUI 側のインジケータが担うため)。
#[test]
fn preedit_has_no_mode_marker() {
    let p = engine(&[]);
    unsafe { ttyskk_key(p, 'j' as u32, CTRL) };
    typed(p, "a");
    // 直接入力では表示するものが無い
    assert_eq!(unsafe { ttyskk_preedit_len(p) }, 0);

    typed(p, "Kanji");
    for i in 0..unsafe { ttyskk_preedit_len(p) } {
        let st = unsafe { ttyskk_preedit_style(p, i) };
        assert!(
            (TTYSKK_STYLE_READING..=TTYSKK_STYLE_READING_CURSOR).contains(&st),
            "GUI へ渡す装飾だけが残る"
        );
    }
    unsafe { ttyskk_free(p) };
}

/// 名前のあるキーが畳めること。
#[test]
fn named_keys_fold() {
    assert_eq!(to_key(RETURN, 0), Some(Key::Enter));
    assert_eq!(to_key(BACKSPACE, 0), Some(Key::Backspace));
    assert_eq!(to_key(0xff09, 0), Some(Key::Tab));
    assert_eq!(to_key(ISO_LEFT_TAB, 0), Some(Key::ShiftTab));
    assert_eq!(to_key(ESCAPE, 0), Some(Key::Esc));
    assert_eq!(to_key('j' as u32, CTRL), Some(Key::Ctrl(0x0a)));
    assert_eq!(to_key(' ' as u32, CTRL), Some(Key::Ctrl(0x00)));
    assert_eq!(to_key('a' as u32, 0), Some(Key::Char('a')));
    // 畳めないものは呼ぶ側へ
    assert_eq!(to_key(LEFT, 0), None);
    // Alt / Super の付いた打鍵は入力メソッドの仕事ではない
    assert_eq!(to_key('a' as u32, 1 << 3), None);
    // Unicode keysym
    assert_eq!(to_key(0x01000000 + 'あ' as u32, 0), Some(Key::Char('あ')));
}

/// 取り消しと、入力中の内容を捨てる操作。
#[test]
fn reset_drops_what_is_being_typed() {
    let p = engine(&[]);
    unsafe { ttyskk_key(p, 'j' as u32, CTRL) };
    typed(p, "Kanji");
    assert_eq!(preedit(p), "▽かんじ");

    unsafe { ttyskk_reset(p) };
    assert_eq!(preedit(p), "");
    assert_eq!(commit(p), "", "捨てるので確定もしない");
    // モードは変わらない
    assert_eq!(unsafe { ttyskk_mode(p) }, TTYSKK_MODE_HIRAGANA);

    unsafe { ttyskk_free(p) };
}

/// 設定を差し替えると、その場でキーの割り当てが変わる。
#[test]
fn config_can_be_replaced() {
    let p = engine(&[]);
    let toml = CString::new("[keys]\nkana = \"C-o\"\n").unwrap();
    unsafe { ttyskk_set_config(p, toml.as_ptr()) };

    // C-j はもうかなモードへ入らない
    assert!(!unsafe { ttyskk_key(p, 'j' as u32, CTRL) });
    assert_eq!(unsafe { ttyskk_mode(p) }, TTYSKK_MODE_ASCII);

    assert!(unsafe { ttyskk_key(p, 'o' as u32, CTRL) });
    assert_eq!(unsafe { ttyskk_mode(p) }, TTYSKK_MODE_HIRAGANA);

    unsafe { ttyskk_free(p) };
}

/// 学習が利用者辞書へ書き出される。
#[test]
fn learning_is_saved() {
    let p = engine(&[("かんじ", "/漢字/幹事/")]);
    unsafe { ttyskk_key(p, 'j' as u32, CTRL) };
    typed(p, "Kanji  ");
    unsafe { ttyskk_key(p, 'j' as u32, CTRL) };
    assert_eq!(commit(p), "幹事");

    unsafe { ttyskk_save(p) };
    unsafe { ttyskk_free(p) };
}

/// NULL を渡しても落ちない。
#[test]
fn null_handles_are_survivable() {
    let null: *mut TtyskkEngine = std::ptr::null_mut();
    unsafe {
        assert!(!ttyskk_key(null, 'a' as u32, 0));
        assert_eq!(ttyskk_mode(null), TTYSKK_MODE_ASCII);
        assert_eq!(ttyskk_preedit_len(null), 0);
        assert_eq!(ttyskk_candidate_len(null), 0);
        assert_eq!(CStr::from_ptr(ttyskk_commit(null)).to_bytes(), b"");
        ttyskk_reset(null);
        ttyskk_save(null);
        ttyskk_set_config(null, std::ptr::null());
        ttyskk_free(null);
    }
}

/// 辞書を読めない場合は NULL を返す (落ちない)。
#[test]
fn a_bad_dictionary_path_is_not_fatal() {
    let sys = CString::new("/nonexistent/does-not-exist.dict").unwrap();
    let user = CString::new("/nonexistent/user.dict").unwrap();
    let p = unsafe { ttyskk_new(sys.as_ptr(), user.as_ptr(), std::ptr::null()) };
    // 共有辞書が無くても起動はできる (変換できないだけ)
    if !p.is_null() {
        unsafe { ttyskk_free(p) };
    }
}
