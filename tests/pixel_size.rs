//! 端末の**画素の寸法**が、包んだ子まで届くこと。
//!
//! 画像を描くアプリ (kitty graphics を使う nvim の画像表示など) は `TIOCGWINSZ` の
//! 画素の寸法を桁数で割って一マスの大きさを出し、画像を何マス分に描くかを決める。
//! **0 だと寸法が 0 になり、黙って何も描かない。** 誤りも警告も出ないので、ttyskk で
//! 包んだ途端に画像だけが消えたように見える。
//!
//! ttyskk はこの値の意味を知らなくてよい。上流から受け取ってそのまま子へ流すだけ。
//! ここで見るのは**起こしたとき**と**大きさが変わったとき**の両方。片方だけ直しても、
//! 分割を動かした拍子に 0 へ戻れば同じことになる。
//!
//! 確かめ方は、子が使っている擬似端末を**こちらから開き直して** `TIOCGWINSZ` を
//! 読む。子の中で道具を走らせる形にすると、その道具 (python など) の有無に
//! 結果が左右される。

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// 上流の端末が名乗る大きさ。24x80 で一マス 16x28 画素のつもり。
const ROWS: u16 = 24;
const COLS: u16 = 80;
const XPIXEL: u16 = COLS * 16;
const YPIXEL: u16 = ROWS * 28;

/// 分割を動かした後の大きさ。桁も画素も変える。
const ROWS2: u16 = 30;
const COLS2: u16 = 100;
const XPIXEL2: u16 = COLS2 * 16;
const YPIXEL2: u16 = ROWS2 * 28;

fn size(rows: u16, cols: u16, xpixel: u16, ypixel: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: xpixel,
        pixel_height: ypixel,
    }
}

/// `pid` が使っている擬似端末の `TIOCGWINSZ`。
///
/// **制御端末を奪わないように `O_NOCTTY` で開く。** 付けないと、この試験プロセスが
/// 子の端末を横取りしかねない。
fn winsize_of(pid: u32) -> Option<(u16, u16, u16, u16)> {
    use std::os::fd::AsRawFd;
    let tty = std::fs::read_link(format!("/proc/{pid}/fd/0")).ok()?;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&tty)
        .ok()?;
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(f.as_raw_fd(), libc::TIOCGWINSZ, &raw mut ws) };
    (rc == 0).then_some((ws.ws_row, ws.ws_col, ws.ws_xpixel, ws.ws_ypixel))
}

use std::os::unix::fs::OpenOptionsExt;

/// ttyskk の直下の子 (シェル) の PID。起きるまで少し待つ。
fn child_of(pid: u32, secs: u64) -> Option<u32> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let out = std::process::Command::new("pgrep")
            .arg("-P")
            .arg(pid.to_string())
            .output()
            .ok()?;
        if let Some(first) = String::from_utf8_lossy(&out.stdout).split_whitespace().next()
            && let Ok(p) = first.parse()
        {
            return Some(p);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// 期待する大きさになるまで待つ。最後に見えた値を返す。
fn wait_for(pid: u32, want: (u16, u16, u16, u16), secs: u64) -> Option<(u16, u16, u16, u16)> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut last = None;
    while Instant::now() < deadline {
        last = winsize_of(pid);
        if last == Some(want) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

#[test]
fn the_pixel_size_reaches_the_child() {
    let dir = std::env::temp_dir().join(format!("ttyskk-pixel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");

    let pty = native_pty_system()
        .openpty(size(ROWS, COLS, XPIXEL, YPIXEL))
        .expect("擬似端末を開けない");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    cmd.args(["--", "sh"]);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", dir.join("no-such-config.toml"));

    let mut child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);
    let wrapper = child.process_id().expect("ttyskk の PID を取れない");

    // 位置の問い合わせに答える係。答えないと ttyskk は控えの原点が定まらず、
    // 起動の流れが変わる。
    let mut reader = pty.master.try_clone_reader().expect("読めない");
    let writer = Arc::new(Mutex::new(pty.master.take_writer().expect("書けない")));
    {
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
            }
        });
    }

    let shell = child_of(wrapper, 10).expect("子のシェルが起きない");

    // 起こしたとき
    let at_start = wait_for(shell, (ROWS, COLS, XPIXEL, YPIXEL), 10);

    // 分割を動かしたとき。上流の大きさを変えると ttyskk に SIGWINCH が飛ぶ。
    pty.master
        .resize(size(ROWS2, COLS2, XPIXEL2, YPIXEL2))
        .expect("大きさを変えられない");
    let after_resize = wait_for(shell, (ROWS2, COLS2, XPIXEL2, YPIXEL2), 10);

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        at_start,
        Some((ROWS, COLS, XPIXEL, YPIXEL)),
        "起こしたときに画素の寸法が子へ届いていない (画像を描くアプリが黙って何も描かなくなる)"
    );
    assert_eq!(
        after_resize,
        Some((ROWS2, COLS2, XPIXEL2, YPIXEL2)),
        "大きさが変わったときに画素の寸法が子へ届いていない"
    );
}
