//! 包まれている側から頼んだ差し替えの合図が、包んでいる ttyskk まで届くこと。
//!
//! 端末の中から `ttyskk --reload` を打つと、その端末を包んでいる ttyskk が合図を
//! 受け取る。**この段では受け取るだけで、まだ差し替えない。** 擬似端末と子を抱えた
//! ままの `exec` はこの次で、そこへ進む前に「合図が端末をまたいで届くか」「主ループ
//! が受け取れるか」を先に確かめておく。
//!
//! 宛先は環境変数 `TTYSKK_ACTIVE` に入れてある親の PID。子は自分を包んでいるものを
//! そこからしか知れないので、その受け渡しごと見張る。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

#[test]
fn the_reload_signal_reaches_the_wrapper() {
    let dir = std::env::temp_dir().join(format!("ttyskk-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");
    let log = dir.join("trace.log");
    let _ = std::fs::remove_file(&log);

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    // 打ち込んだものを読むシェルが要る (実際の使い方と同じく、包まれた端末から呼ぶ)
    cmd.args(["--", "sh"]);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", dir.join("no-such-config.toml"));
    // 受け取ったことは記録にしか出ないので、書き出し先を渡す
    cmd.env("TTYSKK_DEBUG", &log);

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
                if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                    let mut w = writer.lock().unwrap();
                    let _ = w.write_all(b"\x1b[1;1R");
                    let _ = w.flush();
                }
                seen.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
    }

    std::thread::sleep(Duration::from_millis(700));

    // 包まれている側から頼む。子のシェルにそのまま打ち込む。
    // 行末は CR。**LF (0x0a) は Ctrl+J** で、ttyskk はそれをかなモードへ入る合図に
    // 使っている (素の端末でも Enter は CR を送る)。
    // 打鍵の反響は子の出力と混ざるので黙らせる。
    {
        let mut w = writer.lock().unwrap();
        w.write_all(b"stty -echo\r").expect("書けない");
        w.flush().expect("流せない");
    }
    std::thread::sleep(Duration::from_millis(400));
    {
        let mut w = writer.lock().unwrap();
        let line = format!("{} --reload\r", env!("CARGO_BIN_EXE_ttyskk"));
        w.write_all(line.as_bytes()).expect("書けない");
        w.flush().expect("流せない");
    }

    // 記録に出るまで待つ
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut logged = String::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        logged = std::fs::read_to_string(&log).unwrap_or_default();
        if logged.contains("差し替えを頼まれた") {
            break;
        }
    }

    let out = String::from_utf8_lossy(&seen.lock().unwrap().clone()).into_owned();
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        out.contains("差し替えを頼んだ"),
        "頼む側が親を見つけられていない: {out:?}"
    );
    assert!(
        logged.contains("差し替えを頼まれた"),
        "合図が包んでいる ttyskk に届いていない。記録: {logged:?}"
    );
}

/// 包まれていないところで呼んだら、黙って何かを撃たずに断る。
#[test]
fn asking_from_outside_is_refused() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ttyskk"))
        .arg("--reload")
        .env_remove("TTYSKK_ACTIVE")
        .output()
        .expect("起こせない");
    assert!(!out.status.success(), "外から呼べてしまう");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ttyskk の中にいない"), "{err:?}");
}

/// 古い版に包まれているときは、そう分かる断りを返す。
///
/// 古い版は目印に `1` を入れる。**PID として撃つと init を撃つ**ので、数として
/// 読めても 1 以下は宛先にしない。
#[test]
fn an_old_wrapper_is_told_apart() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ttyskk"))
        .arg("--reload")
        .env("TTYSKK_ACTIVE", "1")
        .output()
        .expect("起こせない");
    assert!(!out.status.success(), "古い版の目印で撃ってしまう");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("古く"), "{err:?}");
}
