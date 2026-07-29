//! `C-h` が子アプリまで押されたまま届くことの見張り。
//!
//! `C-h` は ttyskk の中では手前の一文字を消すが、**消すものが無ければ子の持ち物**に
//! なる。ここで `0x7f` にすり替えると、`C-h` に別の働きを割り当てているアプリで
//! それが効かなくなる (nvim の窓の移動が実際にそうだった — `0x08` なら `<C-h>`、
//! `0x7f` なら `<BS>` として読まれる)。
//!
//! 拡張鍵盤プロトコル (Claude Code や nvim が有効にする) の下では `C-h` が
//! `CSI 104;5u` の形で届く。**割り当て一覧に挙げていないと素通りしてしまい、▽ の
//! 途中で押したときに見出し語がそこで確定する**ので、挙げたうえで素通しの筋道だけを
//! 残す必要がある。両方をここで見る。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// 拡張鍵盤プロトコルを頼み、受け取ったバイトを見える形に開いて返す子。
/// `0x08` は `^H`、`0x7f` は `^?` になる。
const CHILD: &str = "printf '\\033[>1u'; stty -echo raw; cat -v";

/// kitty 形式の打鍵。`CSI <記号> ; 5 u` が Ctrl 付き。
const KANA: &[u8] = b"\x1b[106;5u"; // C-j
const CTRL_H: &[u8] = b"\x1b[104;5u";
const CANCEL: &[u8] = b"\x1b[103;5u"; // C-g

#[test]
fn ctrl_h_deletes_in_the_reading_but_belongs_to_the_child_otherwise() {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");

    let dir = std::env::temp_dir().join(format!("ttyskk-ctrlh-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/幹事/\n").expect("辞書を書けない");
    let conf = dir.join("config.toml");
    // 手元の設定に引きずられないよう、既定で走らせる
    std::fs::write(&conf, "").expect("設定を書けない");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    cmd.args(["--", "sh", "-c", CHILD]);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env_remove("TTYSKK_DEBUG");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", &conf);

    let mut child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().expect("読めない");
    let writer = Arc::new(Mutex::new(pty.master.take_writer().expect("書けない")));
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    {
        let (seen, writer) = (seen.clone(), writer.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                    let mut w = writer.lock().unwrap();
                    let _ = w.write_all(b"\x1b[1;1R");
                    let _ = w.flush();
                }
                seen.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
    }

    let key = |k: &[u8]| {
        let mut w = writer.lock().unwrap();
        let _ = w.write_all(k);
        let _ = w.flush();
        drop(w);
        std::thread::sleep(Duration::from_millis(220));
    };
    let take = || {
        let mut s = seen.lock().unwrap();
        let out = String::from_utf8_lossy(&s).into_owned();
        s.clear();
        out
    };

    std::thread::sleep(Duration::from_millis(600));
    key(KANA);
    key(b"Kanji");
    let reading = take();
    assert!(
        reading.contains("▽かんじ"),
        "見出し語が出ていない。試験の組み方を疑う\n  受け取った: {reading:?}"
    );

    // ▽ の途中では ttyskk が消す。子には何も渡さない。
    key(CTRL_H);
    let after = take();
    assert!(
        after.contains("▽かん"),
        "▽ の見出し語から一文字消えていない\n  受け取った: {after:?}"
    );
    assert!(
        !after.contains("^H") && !after.contains("^?"),
        "消したはずなのに子へも渡っている\n  受け取った: {after:?}"
    );

    // 打ちかけを片付けると、消すものが無くなる
    key(CANCEL);
    take();

    // ここからは子の持ち物。**押されたまま `0x08` で届く。**
    key(CTRL_H);
    let passed = take();

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        passed.contains("^H"),
        "C-h が押されたまま子へ届いていない\n  受け取った: {passed:?}"
    );
    assert!(
        !passed.contains("^?"),
        "C-h が Backspace にすり替わっている。nvim の <C-h> が効かなくなる\n  \
         受け取った: {passed:?}"
    );
}
