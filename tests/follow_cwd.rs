//! ttyskk が、包んでいる子の `cd` に付いて自分も移ること。
//!
//! ttyskk は擬似端末をもう一枚開いて子を動かすので、**外側の端末の前景プロセス
//! グループには ttyskk しか居ない**。端末多重化器 (herdr など) はそこから作業
//! ディレクトリを取るため、付いていかないとタブ一覧が起動時の場所に貼り付き、
//! ディレクトリ基準で移動する道具も的を外す。
//!
//! 見るのは二つ。**移ること** (子が `cd` したら外から見える ttyskk の作業
//! ディレクトリも変わる) と、**移っても行き先を見失わないこと** (相対パスで
//! 与えた利用者辞書が、起動した場所に書かれ続ける)。後者が要 — `chdir` は
//! プロセス全体に効くので、ここを踏むと辞書が知らない場所へ散らばる。

#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

/// 外から見える作業ディレクトリ。端末多重化器が見るのと同じところ。
fn cwd_of(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// `pid` の作業ディレクトリが `want` になるまで待つ。最後に見えた値を返す。
fn wait_for_cwd(pid: u32, want: &std::path::Path, secs: u64) -> Option<std::path::PathBuf> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut last = None;
    while Instant::now() < deadline {
        last = cwd_of(pid);
        if last.as_deref() == Some(want) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

/// 位置の問い合わせに答える係を裏で回し、打鍵する手を返す。
///
/// **答えないと ttyskk は控えの原点が定まらず、重ね描きを一切描かない。** そこまで
/// 慎重でなくてよい試験でも、答えておかないと動きが変わる。
fn attend(master: &(dyn MasterPty + Send)) -> impl Fn(&str) {
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

fn open_pty() -> portable_pty::PtyPair {
    native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない")
}

/// 一時の置き場所。`/tmp` が symlink の環境があるので、`/proc` から読んだ値と
/// 比べられるように実体へ解いておく。
fn workspace(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ttyskk-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    dir.canonicalize().expect("実体を辿れない")
}

#[test]
fn the_wrapper_follows_the_child_into_a_new_directory() {
    let base = workspace("follow-cwd");
    let start = base.join("start");
    let moved = base.join("moved");
    std::fs::create_dir_all(&start).expect("起点を作れない");
    std::fs::create_dir_all(&moved).expect("行き先を作れない");
    let dict = base.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");

    let pty = open_pty();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    cmd.args(["--", "sh"]);
    cmd.cwd(&start);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", base.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", base.join("no-such-config.toml"));

    let mut child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);
    let pid = child.process_id().expect("ttyskk の PID を取れない");
    let line = attend(&*pty.master);

    std::thread::sleep(Duration::from_millis(700));
    // 起きた直後は起点に居るはず。ここが違えば、後の比較に意味が無い。
    let at_first = cwd_of(pid);

    // 行末は CR。LF (0x0a) は Ctrl+J で、かなモードへ入る合図になってしまう。
    line("stty -echo\r");
    std::thread::sleep(Duration::from_millis(300));
    // **`cd` を打ったきり放置する。** 続けて何か打てば、そのついでに気づく。
    // 打たずに置いたときに追いつけるかが要 — 端末を放り出したまま席を立つのが、
    // まさに端末多重化器がタブ一覧を作る場面。
    line(&format!("cd {}\r", moved.display()));

    let after = wait_for_cwd(pid, &moved, 10);

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        at_first.as_deref(),
        Some(start.as_path()),
        "起きた場所からして違う"
    );
    assert_eq!(
        after.as_deref(),
        Some(moved.as_path()),
        "子が cd したのに、外から見える ttyskk の作業ディレクトリが付いてこない"
    );
}

/// 移った先に釣られて、辞書の行き先まで変わってしまわないこと。
///
/// `chdir` はプロセス全体に効く。相対パスのまま抱えていると、覚えたことが
/// **その時どこに居たか**で散らばる。起動時に絶対パスへ畳んであれば起きない。
#[test]
fn a_relative_user_dict_stays_where_it_started() {
    let base = workspace("follow-cwd-dict");
    let start = base.join("start");
    let moved = base.join("moved");
    std::fs::create_dir_all(&start).expect("起点を作れない");
    std::fs::create_dir_all(&moved).expect("行き先を作れない");
    let dict = base.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");

    let pty = open_pty();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ttyskk"));
    cmd.args(["--", "sh"]);
    cmd.cwd(&start);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env("TTYSKK_JISYO", &dict);
    // **相対パス。** 起点で畳まれていなければ、移った先に書かれる。
    cmd.env("TTYSKK_USER_JISYO", "user.dict");
    cmd.env("TTYSKK_CONFIG", base.join("no-such-config.toml"));

    let mut child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);
    let pid = child.process_id().expect("ttyskk の PID を取れない");
    let key = attend(&*pty.master);

    std::thread::sleep(Duration::from_millis(700));
    key("stty -echo\r");
    std::thread::sleep(Duration::from_millis(300));
    key(&format!("cd {}\r", moved.display()));

    // 先に移っていることを確かめる。移っていなければ、この試験は何も見張れない。
    let followed = wait_for_cwd(pid, &moved, 10);

    // 変換して確定する。**候補を選んだ記録が残らないと辞書は書かれない** ので、
    // ただの仮名では足りない。
    key("\x0a"); // C-j でかなモードへ
    std::thread::sleep(Duration::from_millis(200));
    key("Kanji \r");

    // 手が止まってから書き出すので、待ちは保存の間 (3 秒) より長く取る。
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        if start.join("user.dict").exists() || moved.join("user.dict").exists() {
            break;
        }
    }
    let at_start = start.join("user.dict").exists();
    let at_moved = moved.join("user.dict").exists();

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        followed.as_deref(),
        Some(moved.as_path()),
        "そもそも付いていっていない (この試験は空回りしている)"
    );
    assert!(
        !at_moved,
        "移った先に利用者辞書を書いてしまった (相対パスが畳まれていない)"
    );
    assert!(at_start, "起動した場所に利用者辞書が書かれていない");
}
