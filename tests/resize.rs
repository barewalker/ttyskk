//! 画面の大きさが変わったとき (多重化の分割を増やす・解除する) の見張り。
//!
//! ttyskk は大きさが変わるとカーソル位置を端末に尋ねる (`CSI 6 n`)。その返事は
//! 端末から**入力として**返ってくるので、ttyskk が取り除かないと子アプリへ
//! そのまま流れ込む。子は `CSI 12;1R` を打たれたものとして受け取り、▽ の途中
//! だったなら**見出し語がそこで確定して流れ出る**。
//!
//! tmux や herdr で分割を変えるとアプリは必ず画面を描き直す。その出力が返事より
//! 先に届くのが常なので、描き直しを合図に待つのをやめると毎回これが起きる。
//! ここでは**わざと返事を遅らせて**その順番を作り、漏れないことを確かめる。

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// 子。絶えず何か書きながら、受け取ったバイトを `cat -v` で見える形に戻す。
///
/// `cat -v` は非 ASCII を `M-c` のような ASCII に開くので、**子へ流れ込んだもの**と
/// **ttyskk が画面に描いたもの**を端末の出力だけで見分けられる。
const CHILD: &str = "stty -echo raw; (while :; do printf .; sleep 0.05; done) & cat -v";

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[test]
fn resizing_never_leaks_the_cursor_report_into_the_child() {
    let pty = native_pty_system()
        .openpty(size(24, 80))
        .expect("擬似端末を開けない");

    let dir = std::env::temp_dir().join(format!("ttyskk-resize-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/幹事/\n").expect("辞書を書けない");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    cmd.args(["--", "sh", "-c", CHILD]);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env_remove("TTYSKK_DEBUG");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));

    let mut child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().expect("読めない");
    let writer = Arc::new(Mutex::new(pty.master.take_writer().expect("書けない")));

    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    // 大きさを変えたあとは返事を遅らせ、子の描き直しを先に届かせる
    let slow = Arc::new(AtomicBool::new(false));
    {
        let (seen, slow, writer) = (seen.clone(), slow.clone(), writer.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                seen.lock().unwrap().extend_from_slice(&buf[..n]);
                if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                    if slow.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(150));
                    }
                    let mut w = writer.lock().unwrap();
                    let _ = w.write_all(b"\x1b[12;1R");
                    let _ = w.flush();
                }
            }
        });
    }

    let key = |k: &[u8]| {
        let mut w = writer.lock().unwrap();
        let _ = w.write_all(k);
        let _ = w.flush();
        std::thread::sleep(Duration::from_millis(250));
    };

    std::thread::sleep(Duration::from_millis(700));
    key(b"\x0a"); // かなモードへ
    key(b"Kanji"); // ▽かんじ

    // ここから見る。分割を増やして (狭くして) から、解除して (広げて) 戻す。
    seen.lock().unwrap().clear();
    slow.store(true, Ordering::Release);
    for s in [size(12, 40), size(40, 120)] {
        pty.master.resize(s).expect("大きさを変えられない");
        std::thread::sleep(Duration::from_millis(700));
    }

    let got = seen.lock().unwrap().clone();
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);

    let text = String::from_utf8_lossy(&got);
    // 子が受け取ったものは `cat -v` で開かれて戻ってくる
    assert!(
        !text.contains("^[["),
        "位置報告が子アプリへ打鍵として流れ込んだ\n  受け取った: {:?}",
        text.chars().take(400).collect::<String>()
    );
    assert!(
        !text.contains("M-c"),
        "▽ の見出し語が勝手に確定して子アプリへ流れ出た\n  受け取った: {:?}",
        text.chars().take(400).collect::<String>()
    );
    // 大きさが変わっても打ちかけは消えない。新しい場所に描き直されるだけ。
    assert!(
        text.contains("▽かんじ"),
        "大きさが変わったあと、打ちかけの見出し語が描き直されていない\n  受け取った: {:?}",
        text.chars().take(400).collect::<String>()
    );
}
