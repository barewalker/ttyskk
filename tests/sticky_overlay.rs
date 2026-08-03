//! 前置キーの印が、**実際に端末へ描かれる**ことを見張る。
//!
//! エンジンの試験 (`Skk::preedit`) が通っていても、端末に出るとは限らない。
//! 重ね描きは控えの原点が定まって初めて描かれ、それより手前で握り潰される経路が
//! いくつもある。ここでは擬似端末を一枚立てて ttyskk を丸ごと走らせ、`;` を押した
//! ときに `▽` が本当に画面へ出るかを見る。
//!
//! 比べるものとして、同じ手順で `K` (Shift 版) も打つ。あちらで `▽` が出て
//! こちらで出ないなら、印だけが落ちていると分かる。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

#[test]
fn the_sticky_hint_reaches_the_terminal() {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");

    let dir = std::env::temp_dir().join(format!("ttyskk-sticky-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");
    // 前置キーは既定で割り当てが無いので、ここで割り当てる
    let conf = dir.join("config.toml");
    std::fs::write(&conf, "[keys]\nsticky = \";\"\n").expect("設定を書けない");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    cmd.args(["--", "sh", "-c", "stty -echo; sleep 30"]);
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
        let seen = seen.clone();
        let writer = writer.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                // 位置の問い合わせに答えないと、控えの原点が定まらず何も描かれない
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
    };
    // いままでに届いた分を捨てて、次の打鍵の結果だけを見る
    let take = || {
        std::thread::sleep(Duration::from_millis(400));
        let mut s = seen.lock().unwrap();
        String::from_utf8_lossy(&std::mem::take(&mut *s)).into_owned()
    };

    std::thread::sleep(Duration::from_millis(700));
    key(b"\x0a"); // かなモードへ
    let _ = take();

    // 比べるもの: Shift 版なら ▽ が出る
    key(b"K");
    let with_shift = take();
    key(b"\x07"); // C-g で取り消す
    let _ = take();

    // 本題: 前置キーでも ▽ が出るか
    key(b";");
    let with_sticky = take();

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        with_shift.contains('▽'),
        "Shift でも ▽ が出ていない。試験の作りが悪い: {with_shift:?}"
    );
    assert!(
        with_sticky.contains('▽'),
        "前置キーを押しても ▽ が端末に出ない: {with_sticky:?}"
    );
}
