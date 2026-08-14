//! 端末から来たものが、包んだ子までそのまま届くこと。
//!
//! ttyskk は打鍵を横取りする。**横取りしてよいのは押された鍵だけ**で、端末が返して
//! きた応答や、上流が決めた端末の設定まで変えてしまってはいけない。ここが崩れると、
//! 子アプリからは「ttyskk を通した途端に端末が別物になった」ように見える。
//!
//! 見るのは三つ。
//!
//! 1. 文字列を伴う列 (`OSC` / `DCS` / `SOS` / `PM` / `APC`) が中身ごと素通しされること
//! 2. 辞書登録の途中でも端末の応答が子へ届くこと
//! 3. 上流の端末設定 (termios) が子へ引き継がれること

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// 受け取ったものをそのまま見える形で返す子。
///
/// `cat -v` は制御文字を `^[` のような ASCII に開くので、**子へ流れ込んだもの**を
/// 端末の出力だけで確かめられる。
const CHILD: &str = "stty -echo raw; cat -v";

struct Wrapped {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    seen: Arc<Mutex<Vec<u8>>>,
    /// 持っているだけ。落とすと擬似端末が閉じる。
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Wrapped {
    fn send(&self, b: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        w.write_all(b).unwrap();
        w.flush().unwrap();
    }

    /// 子の出力に `want` が現れるまで待つ。現れたら true。
    fn wait_for(&self, want: &str, secs: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            let got = String::from_utf8_lossy(&self.seen.lock().unwrap().clone()).into_owned();
            if got.contains(want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn seen(&self) -> String {
        String::from_utf8_lossy(&self.seen.lock().unwrap().clone()).into_owned()
    }

    fn forget(&self) {
        self.seen.lock().unwrap().clear();
    }
}

impl Drop for Wrapped {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// ttyskk で `CHILD` を包んで起こす。`tweak` は擬似端末を開いた直後に呼ばれる。
fn wrap(tag: &str, child_cmd: &str, tweak: impl FnOnce(&dyn portable_pty::MasterPty)) -> Wrapped {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");

    // 上流の端末を整えてから包む。ttyskk はここを控えて子へ渡すはず。
    tweak(&*pty.master);

    let dir = std::env::temp_dir().join(format!("ttyskk-trans-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(&dict, ";; okuri-nasi entries.\n").expect("辞書を書けない");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    cmd.args(["--", "sh", "-c", child_cmd]);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env_remove("TTYSKK_DEBUG");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", dir.join("no-such-config.toml"));
    // **HOME も差し替える。** そのままだと実環境の fcitx5 の辞書
    // (`~/.local/share/fcitx5/skk/user.dict`) を取り込んでしまい、空辞書のつもりが
    // 変換できてしまう。
    cmd.env("HOME", &dir);
    // カーソルの塗り替えを止める。子へ届いた分だけを見たい。
    cmd.env("TTYSKK_NO_CURSOR", "1");

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
                seen.lock().unwrap().extend_from_slice(&buf[..n]);
                // 位置の問い合わせには必ず答える。答えないと ttyskk は控えの原点が
                // 定まらず、重ね描きを一切描かないので流れが変わる。
                if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                    let mut w = writer.lock().unwrap();
                    let _ = w.write_all(b"\x1b[1;1R");
                    let _ = w.flush();
                }
            }
        });
    }

    let w = Wrapped {
        child,
        writer,
        seen,
        _master: pty.master,
    };
    std::thread::sleep(Duration::from_millis(700));
    w
}

/// 文字列を伴う列が、中身ごとそのまま子へ届くこと。
///
/// **かなモードで確かめる。** ASCII では素通しなので差が出ない。`ESC ]` (OSC) しか
/// 一塊として切り出していないと、`ESC P` や `ESC _` の本体が普通の打鍵として変換に
/// かかり、`kitty` が `きっty` になって届く。`ESC _` (kitty graphics の応答) に
/// 至っては本体が辞書登録の見出し語に吸い込まれ、画面まで荒れる。
#[test]
fn the_string_sequences_reach_the_child_intact() {
    let w = wrap("strings", CHILD, |_| {});

    // かなモードへ。ここが要 — ASCII のままでは何も証明できない。
    w.send(b"\x0a");
    std::thread::sleep(Duration::from_millis(250));

    let cases: [(&str, &[u8], &str); 5] = [
        (
            "DCS (XTVERSION の応答)",
            b"\x1bP>|kitty(0.42.2)\x1b\\",
            "^[P>|kitty(0.42.2)^[\\",
        ),
        (
            "APC (kitty graphics の応答)",
            b"\x1b_Gi=1,a=q;OK\x1b\\",
            "^[_Gi=1,a=q;OK^[\\",
        ),
        ("PM", b"\x1b^hello\x1b\\", "^[^hello^[\\"),
        ("SOS", b"\x1bXhello\x1b\\", "^[Xhello^[\\"),
        (
            "OSC (色の応答)",
            b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\",
            "^[]11;rgb:1e1e/1e1e/2e2e^[\\",
        ),
    ];

    for (name, seq, want) in cases {
        w.forget();
        w.send(seq);
        assert!(
            w.wait_for(want, 5),
            "{name} が中身ごと届いていない。子が受け取ったもの: {:?}",
            w.seen()
        );
    }
}

/// 辞書登録の途中でも、端末の応答は子へ届くこと。
///
/// 登録中の `Key::Raw` は「矢印を挟むと登録内容が壊れる」ので捨てているが、**応答まで
/// 一緒に捨ててはいけない**。子アプリが自分で尋ねた結果なので、消すと問い合わせた側が
/// 待ちぼうけになる。登録に入っているかどうかは子の知らない事情。
#[test]
fn a_terminal_reply_survives_the_registration() {
    let w = wrap("reply", CHILD, |_| {});

    // 候補の無い語を変換して、辞書登録に入る
    w.send(b"\x0a");
    std::thread::sleep(Duration::from_millis(250));
    w.send(b"Kanji ");
    assert!(
        w.wait_for("登録:かんじ", 5),
        "辞書登録に入っていない。画面: {:?}",
        w.seen()
    );

    // 端末の応答はここでも通す
    for (name, seq, want) in [
        ("装置属性 (DA1)", &b"\x1b[?62;1;6c"[..], "^[[?62;1;6c"),
        ("窓の大きさ", &b"\x1b[6;30;14t"[..], "^[[6;30;14t"),
        (
            "kitty graphics",
            &b"\x1b_Gi=1,a=q;OK\x1b\\"[..],
            "^[_Gi=1,a=q;OK^[\\",
        ),
    ] {
        w.forget();
        w.send(seq);
        assert!(
            w.wait_for(want, 5),
            "登録中に {name} の応答が消えた。画面: {:?}",
            w.seen()
        );
    }

    // 一方、矢印は従来どおり通さない (登録内容が壊れるため)
    w.forget();
    w.send(b"\x1b[A");
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !w.seen().contains("^[[A"),
        "矢印まで子へ流れている。画面: {:?}",
        w.seen()
    );
}

/// 上流の端末設定が、子の擬似端末へ引き継がれること。
///
/// ttyskk は自分の端末を raw にしてしまうので、控えた原本を渡さないと子は
/// `openpty` の既定値で始まる。`stty` で整えてから包んでも設定が戻ったように見える。
#[test]
fn the_terminal_settings_are_handed_down() {
    // 上流をわざと既定から外す。消し文字を `^H` に、`IUTF8` を落とす。
    let tweak = |m: &dyn portable_pty::MasterPty| {
        let fd = m.as_raw_fd().expect("擬似端末の fd を取れない");
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        unsafe { libc::tcgetattr(fd, &raw mut t) };
        t.c_cc[libc::VERASE] = 0x08;
        t.c_iflag &= !libc::IUTF8;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const t) };
    };

    // 子は起きたまま何もしない。`stty` で触ると、引き継がれたかが分からなくなる。
    let w = wrap("termios", "exec sleep 30", tweak);

    let wrapper = w.child.process_id().expect("ttyskk の PID を取れない");
    let shell = {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut found = None;
        while Instant::now() < deadline && found.is_none() {
            if let Ok(o) = std::process::Command::new("pgrep")
                .arg("-P")
                .arg(wrapper.to_string())
                .output()
                && let Some(f) = String::from_utf8_lossy(&o.stdout).split_whitespace().next()
                && let Ok(p) = f.parse::<u32>()
            {
                found = Some(p);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        found.expect("子が起きない")
    };

    // 子が使っている擬似端末を、こちらから開き直して読む。
    // **制御端末を奪わないよう `O_NOCTTY`。**
    let tty = std::fs::read_link(format!("/proc/{shell}/fd/0")).expect("子の端末を辿れない");
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&tty)
        .expect("子の端末を開けない");
    let mut got: libc::termios = unsafe { std::mem::zeroed() };
    unsafe { libc::tcgetattr(f.as_raw_fd(), &raw mut got) };

    // 上流をどう変えたかに関わらず、ttyskk 自身は raw のまま動く。ここで見るのは
    // **子が受け取った設定**だけ。
    drop(w);

    assert_eq!(
        got.c_cc[libc::VERASE],
        0x08,
        "上流の消し文字 (^H) が子へ引き継がれていない"
    );
    assert_eq!(
        got.c_iflag & libc::IUTF8,
        0,
        "上流で落とした IUTF8 が子で立っている"
    );
}
