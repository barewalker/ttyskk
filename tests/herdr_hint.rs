//! 中で前景にいるものの名前を、herdr へ `HERDR_AGENT` で知らせること。
//!
//! herdr はエージェントへ文を送る前に、ペインの前景の処理がそのエージェントかを
//! 確かめる。ttyskk に包まれていると前景は ttyskk なので、何も送れない。herdr は
//! 前景の処理の `/proc/<pid>/environ` に `HERDR_AGENT=<名前>` があればそれを信じる
//! ので、ttyskk は中の前景に合わせてこの値を書き換える。
//!
//! 見るのは herdr と同じところ (外から読んだ `/proc/<pid>/environ`)。自分の中で
//! 値を変えても、ここに映らなければ herdr には届かない。

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

/// 外から見える `HERDR_AGENT`。herdr と同じく、NUL で区切った最初の一つを取る。
fn agent_of(pid: u32) -> Option<String> {
    let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    env.split(|&b| b == 0).find_map(|rec| {
        rec.strip_prefix(b"HERDR_AGENT=")
            .map(|v| String::from_utf8_lossy(v).into_owned())
    })
}

/// `HERDR_AGENT` が `want` を満たすまで待つ。最後に見えた値を返す。
fn wait_for(pid: u32, secs: u64, want: impl Fn(Option<&str>) -> bool) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut last = None;
    while Instant::now() < deadline {
        last = agent_of(pid);
        if want(last.as_deref()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

/// 位置の問い合わせに答える係を裏で回し、打鍵する手を返す。
fn attend(master: &(dyn MasterPty + Send)) -> impl Fn(&str) + use<> {
    let mut reader = master.try_clone_reader().expect("読めない");
    let writer = Arc::new(Mutex::new(master.take_writer().expect("書けない")));
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
    move |s: &str| {
        let mut w = writer.lock().unwrap();
        w.write_all(s.as_bytes()).expect("書けない");
        w.flush().expect("流せない");
    }
}

fn workspace(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ttyskk-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    dir.canonicalize().expect("実体を辿れない")
}

/// 起こした ttyskk。擬似端末の親側を落とすと子が SIGHUP で死ぬので一緒に抱える。
type Started = (
    Box<dyn portable_pty::Child + Send + Sync>,
    u32,
    Box<dyn MasterPty + Send>,
);

/// ttyskk に `sh` を包ませて起こす。`pane` が真なら herdr のペインの中を装う。
fn start(base: &std::path::Path, pane: bool) -> Started {
    let dict = base.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    cmd.args(["--", "sh"]);
    cmd.cwd(base);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env_remove("HERDR_AGENT");
    cmd.env_remove("HERDR_PANE_ID");
    if pane {
        cmd.env("HERDR_PANE_ID", "w1:p1");
    }
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", base.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", base.join("no-such-config.toml"));
    let child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);
    let pid = child.process_id().expect("ttyskk の PID を取れない");
    (child, pid, pty.master)
}

#[test]
fn the_foreground_agent_is_told_to_herdr() {
    let base = workspace("herdr-hint");
    // 名前だけ claude の sleep。herdr は名前しか見ない。
    let fake = base.join("claude");
    std::os::unix::fs::symlink("/bin/sleep", &fake).expect("偽の claude を置けない");

    let (mut child, pid, master) = start(&base, true);
    let key = attend(&*master);
    std::thread::sleep(Duration::from_millis(700));
    // 待っている間の前景はシェル。herdr は `sh` を知らない名前として読み飛ばす。
    let idle = wait_for(pid, 5, |v| v == Some("sh"));

    // 行末は CR。LF は Ctrl+J で、かなモードへ入る合図になってしまう。
    key("stty -echo\r");
    std::thread::sleep(Duration::from_millis(300));
    // 子に枠が漏れていないかを、ついでに書き出させる
    key(&format!("echo \"[$HERDR_AGENT]\" > {}\r", base.join("seen").display()));
    std::thread::sleep(Duration::from_millis(300));
    key(&format!("{} 30\r", fake.display()));
    let running = wait_for(pid, 10, |v| v == Some("claude"));

    // C-c で止める。シェルが前景へ戻れば、名前もシェルに戻る。
    key("\x03");
    let back = wait_for(pid, 10, |v| v == Some("sh"));
    let seen = std::fs::read_to_string(base.join("seen")).unwrap_or_default();

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(idle.as_deref(), Some("sh"), "herdr のペインの中なのに前景を知らせていない");
    assert_eq!(running.as_deref(), Some("claude"), "前景の claude が知らされない");
    assert_eq!(back.as_deref(), Some("sh"), "claude が止まったのに名乗り続けている");
    assert_eq!(seen.trim(), "[]", "枠が子に漏れている");
}

/// herdr の外では何も足さない。
#[test]
fn no_slot_outside_herdr() {
    let base = workspace("herdr-hint-outside");
    let (mut child, pid, master) = start(&base, false);
    let _key = attend(&*master);
    std::thread::sleep(Duration::from_millis(1000));
    let v = agent_of(pid);
    let alive = std::fs::metadata(format!("/proc/{pid}")).is_ok();
    let _ = child.kill();
    let _ = child.wait();
    assert!(alive, "ttyskk が起きていない (この試験は空回りしている)");
    assert_eq!(v, None);
}

/// 差し替え (`ttyskk --reload`) の後も知らせ続けること。
///
/// 差し替えの直前、枠には名前が書き込まれていて短い。そのまま次の版へ渡すと
/// 空き枠と見なされず、以後は何も知らせなくなる。
#[test]
fn the_slot_survives_a_reload() {
    let base = workspace("herdr-hint-reload");
    let fake = base.join("claude");
    std::os::unix::fs::symlink("/bin/sleep", &fake).expect("偽の claude を置けない");

    let (mut child, pid, master) = start(&base, true);
    let key = attend(&*master);
    std::thread::sleep(Duration::from_millis(700));
    key("stty -echo\r");
    let before = wait_for(pid, 5, |v| v == Some("sh"));

    // 差し替えの合図。PID は変わらない。
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGUSR2) };
    std::thread::sleep(Duration::from_millis(1000));
    key(&format!("{} 30\r", fake.display()));
    let after = wait_for(pid, 10, |v| v == Some("claude"));
    key("\x03");

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(before.as_deref(), Some("sh"), "差し替える前から知らせていない");
    assert_eq!(after.as_deref(), Some("claude"), "差し替えた後に知らせなくなった");
}
