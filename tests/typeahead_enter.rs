//! ttyskk が起きる前に打たれた命令が、Enter まで含めてそのまま実行されること。
//!
//! herdr は再起動のあと、ペインのシェルを起こすと同時に `claude --resume <ID>` と
//! Enter を書き込む。ttyskk が端末を raw にする前なので、端末は Enter (CR) を LF に
//! 変えて溜める。LF は ttyskk にとって Ctrl+J (かなモードへ入るキー) なので、
//! 戻さないと命令は打たれたまま実行されず、かなモードに入るだけになる。

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

#[test]
fn a_command_typed_before_startup_runs() {
    let base = std::env::temp_dir().join(format!("ttyskk-typeahead-{}", std::process::id()));
    std::fs::create_dir_all(&base).expect("一時の置き場所を作れない");
    let dict = base.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");
    let done = base.join("done");
    let _ = std::fs::remove_file(&done);

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");
    // **起こす前に書く。** herdr と同じ順番。
    let mut writer = pty.master.take_writer().expect("書けない");
    writer
        .write_all(format!("echo resumed > {}\r", done.display()).as_bytes())
        .expect("書けない");
    writer.flush().expect("流せない");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    cmd.args(["--", "sh"]);
    cmd.cwd(&base);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env_remove("HERDR_PANE_ID");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", base.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", base.join("no-such-config.toml"));
    let mut child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);

    // 位置の問い合わせに答える (答えないと 1.5 秒待たされるだけで、結果は同じ)
    let mut reader = pty.master.try_clone_reader().expect("読めない");
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                let _ = writer.write_all(b"\x1b[5;20R");
                let _ = writer.flush();
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !done.exists() {
        std::thread::sleep(Duration::from_millis(100));
    }
    let ran = done.exists();

    let _ = child.kill();
    let _ = child.wait();

    assert!(ran, "起動前に打たれた命令が実行されない (Enter が Ctrl+J になっている)");
}
