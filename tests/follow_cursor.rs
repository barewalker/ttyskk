//! 子アプリのカーソルの形に合わせて、かなモードを降ろす見張り。
//!
//! vim / nvim は挿入モードで棒 (`CSI 6 SP q`)、ノーマルモードでブロック
//! (`CSI 2 SP q`) にする。`ascii_keys` は**押されたキー**を見張るので、`Esc` 以外の
//! 抜け方 (割り当て・コマンド・プラグイン) には付いていけない。形を見れば抜け方に
//! 依らない。
//!
//! nvim は要らない。形を出すだけの子で同じことを確かめられる。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// 棒 (挿入モード) で始め、しばらくしてブロック (ノーマルモード) に変える子。
///
/// 形を変える係は後ろに回し、前では `cat -v` で受け取ったものを見せる。非 ASCII は
/// `M-c…` に開かれるので、「ka」がそのまま出れば ASCII モード、`M-c…` なら
/// **か が届いた** = かなモードのまま、と読み分けられる。
const CHILD: &str = "stty -echo raw; (printf '\\033[6 q'; sleep 1.5; printf '\\033[2 q') & cat -v";

struct Session {
    dir: std::path::PathBuf,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    seen: Arc<Mutex<Vec<u8>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Session {
    fn start(follow: bool, tag: &str) -> Session {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("擬似端末を開けない");

        let dir = std::env::temp_dir().join(format!("ttyskk-cursor-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
        let dict = dir.join("sys.dict");
        std::fs::write(&dict, "かんじ /漢字/幹事/\n").expect("辞書を書けない");
        let conf = dir.join("config.toml");
        std::fs::write(
            &conf,
            format!("[behavior]\nfollow_cursor_shape = {follow}\n"),
        )
        .expect("設定を書けない");

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
        cmd.args(["--", "sh", "-c", CHILD]);
        cmd.env_remove("TTYSKK_ACTIVE");
        cmd.env_remove("TTYSKK_DEBUG");
        cmd.env("TTYSKK_JISYO", &dict);
        cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));
        cmd.env("TTYSKK_CONFIG", &conf);

        let child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
        drop(pty.slave);

        let mut reader = pty.master.try_clone_reader().expect("読めない");
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pty.master.take_writer().expect("書けない")));
        let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
        {
            let (seen, writer) = (seen.clone(), writer.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    // 位置を尋ねられたら答える。答えないと重ね描きが始まらない。
                    if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                        let mut w = writer.lock().unwrap();
                        let _ = w.write_all(b"\x1b[1;1R");
                        let _ = w.flush();
                    }
                    seen.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
        }
        std::thread::sleep(Duration::from_millis(600));
        Session {
            dir,
            child,
            seen,
            writer,
        }
    }

    fn key(&self, k: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(k);
        let _ = w.flush();
        drop(w);
        std::thread::sleep(Duration::from_millis(250));
    }

    /// これまでに受け取ったものを捨て、以後の分だけを見る。
    fn forget(&self) {
        self.seen.lock().unwrap().clear();
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.seen.lock().unwrap()).into_owned()
    }

    fn finish(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// ノーマルモードへ戻ったら、かなモードも降りる。
#[test]
fn a_block_cursor_puts_ttyskk_back_into_ascii() {
    let mut s = Session::start(true, "on");
    s.key(b"\x0a"); // C-j でかなモードへ
    s.key(b"ai"); // あい (打ちかけを残さない)

    // 子がブロックに変えるまで待つ
    std::thread::sleep(Duration::from_millis(1400));
    s.forget();
    s.key(b"ka");
    let got = s.text();
    s.finish();

    assert!(
        got.contains("ka"),
        "ASCII モードへ降りていない (子に ka が素通りしていない)\n  受け取った: {got:?}"
    );
    assert!(
        !got.contains("M-c"),
        "かなモードのまま残っている (子に か が届いた)\n  受け取った: {got:?}"
    );
}

/// 既定では形を見ない。押されたキーだけが頼り。
#[test]
fn the_cursor_shape_is_ignored_unless_asked_for() {
    let mut s = Session::start(false, "off");
    s.key(b"\x0a");
    s.key(b"ai");

    std::thread::sleep(Duration::from_millis(1400));
    s.forget();
    s.key(b"ka");
    let got = s.text();
    s.finish();

    assert!(
        got.contains("M-c"),
        "既定なのに形で降りてしまった (子に か が届いていない)\n  受け取った: {got:?}"
    );
}

/// **打ちかけがあるうちは降ろさない。** 変換の途中で消えると打ち込みが失われる。
#[test]
fn a_conversion_in_flight_is_never_dropped() {
    let mut s = Session::start(true, "busy");
    s.key(b"\x0a");
    s.key(b"Kanji"); // ▽かんじ のまま置いておく

    std::thread::sleep(Duration::from_millis(1400));
    s.forget();
    s.key(b" "); // 変換できるなら ▼漢字 になる
    let got = s.text();
    s.finish();

    assert!(
        got.contains("▼漢字"),
        "変換の途中でモードを降ろされた\n  受け取った: {got:?}"
    );
}
