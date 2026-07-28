//! 子の出力の途中に、重ね描きが割り込まないことを見張る。
//!
//! ttyskk は子の出力の隙間へ自分のバイト列を挟む。**文字やエスケープ列の途中に
//! 挟むと、端末はそこで文字を壊す** (置換文字 U+FFFD になる)。列の切れ目かどうかは
//! `SeqTracker` が見ているが、その判定を通さずに書く経路がいくつも残っていた。
//!
//! ここでは擬似端末を一枚立てて ttyskk を丸ごと走らせ、**端末の役として受け取った
//! すべてのバイトが UTF-8 として妥当か**を見る。中の作りに踏み込まないので、
//! 割り込む経路がまた増えても捕まえられる。
//!
//! 子には日本語を「文字の途中で切れる大きさ」で小刻みに書かせ、その最中に打鍵を
//! 混ぜて重ね描きを何度も起こす。

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// 日本語を**バイト単位で 5〜7 バイトずつ**書き出す sh の中身を組み立てる。
///
/// かなは 3 バイトなので、3 の倍数を避けて切ると必ず文字の途中で切れる。
/// そこが割り込みの起きる場所。`sleep` を挟んで読み込みを小刻みに分ける
/// (一度に届いてしまうと切れ目ができない)。
fn dribble_script() -> String {
    let text =
        "あちらのローカルが wsl を見ているはずなので、git switch main で今回の分が入ります。"
            .repeat(40);
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let n = if (i / 7) % 3 != 0 { 7 } else { 5 };
        let end = (i + n).min(bytes.len());
        out.push_str("printf '");
        for b in &bytes[i..end] {
            out.push_str(&format!("\\{b:03o}"));
        }
        out.push_str("'\nsleep 0.004\n");
        i = end;
    }
    out.push_str("printf '\\r\\n[done]\\r\\n'\nsleep 1\n");
    out
}

/// **いまは通らない。** 割り込む経路をいくつも塞いだが、まだ残っている
/// (この試験で 18 箇所)。塞ぎきったらこの `ignore` を外して、`cargo test` の
/// 並びに戻すこと。
///
/// 走らせるには `cargo test -- --ignored`。
#[test]
#[ignore = "重ね描きの割り込みがまだ残っている (既知の残件)"]
fn the_overlay_never_splits_a_character() {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");

    // 変換が起きないと ▼ の描き直しが起きず、割り込む機会が生まれない。
    // 環境に左右されないよう、その場で小さな辞書を作って使う。
    let dir = std::env::temp_dir().join(format!("ttyskk-race-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(
        &dict,
        "かんじ /漢字/幹事/監事/\nにほんご /日本語/\nあいさつ /挨拶/\nかいぎ /会議/開議/\n",
    )
    .expect("辞書を書けない");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    let script = dribble_script();
    cmd.args(["--", "sh", "-c", &script]);
    // 入れ子回避で素通しされると、ttyskk を通らない試験になってしまう
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env_remove("TTYSKK_DEBUG");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));

    let mut child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().expect("読めない");
    let mut writer = pty.master.take_writer().expect("書けない");

    // 端末の役として受け取ったすべて
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    {
        let seen = seen.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                seen.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
    }

    // 起動を待ち、かなモードへ入る
    std::thread::sleep(Duration::from_millis(600));
    let _ = writer.write_all(b"\x0a");
    let _ = writer.flush();

    // 子が書いている最中に打鍵を混ぜる。重ね描きが何度も起きる。
    let keys = b"Kanji Nihongo Aisatu Kaigi \r";
    let deadline = Instant::now() + Duration::from_secs(20);
    for k in keys.iter().cycle().take(keys.len() * 14) {
        if Instant::now() > deadline {
            break;
        }
        let _ = writer.write_all(&[*k]);
        let _ = writer.flush();
        std::thread::sleep(Duration::from_millis(40));
    }

    std::thread::sleep(Duration::from_millis(800));
    let _ = child.kill();
    let _ = child.wait();
    std::thread::sleep(Duration::from_millis(200));

    let _ = std::fs::remove_dir_all(&dir);
    let got = seen.lock().unwrap().clone();
    assert!(
        got.len() > 3000,
        "子の出力が届いていない ({} バイト)。試験の組み方を疑う",
        got.len()
    );

    if let Err(e) = std::str::from_utf8(&got) {
        let at = e.valid_up_to();
        let lo = at.saturating_sub(40);
        let hi = (at + 40).min(got.len());
        panic!(
            "端末が受け取った列が {at} バイト目で壊れている\n  \
             直前: {:?}\n  ここ: {:?}\n  直後: {:?}\n  \
             壊れる箇所は全部で {} 個",
            String::from_utf8_lossy(&got[lo..at]),
            &got[at..(at + 3).min(got.len())],
            String::from_utf8_lossy(&got[(at + 1).min(got.len())..hi]),
            String::from_utf8_lossy(&got).matches('\u{fffd}').count(),
        );
    }
}
