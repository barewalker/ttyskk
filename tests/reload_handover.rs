//! 差し替えで、子を生かしたまま ttyskk だけが入れ替わること。
//!
//! 見張るのは三つ。**子が生き残る** (同じ PID のまま動き続ける)、**新しい実体に
//! 入れ替わる** (置き換えたバイナリが起きる)、**端末が壊れない** (raw のまま置き
//!去りにされない)。
//!
//! 入れ替わったことは版の文字列では見分けられない (同じ版のまま中身だけ差し替わる
//! ことがある) ので、**バイナリそのものを差し替えて**確かめる。差し替え先は、起こ
//! されたら目印のファイルを書いて子を待つだけの小さな shell script にしてある。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// `path` に文字列が現れるまで待つ。
fn wait_for_file(path: &std::path::Path, secs: u64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(path)
            && !s.trim().is_empty()
        {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

#[test]
fn the_child_survives_and_the_binary_is_replaced() {
    let dir = std::env::temp_dir().join(format!("ttyskk-handover-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");
    let log = dir.join("trace.log");
    let landed = dir.join("landed");
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&landed);

    // **本物を置き場所ごと写す。** 差し替え先はこのパスなので、ここを後から
    // すげ替えれば「新しい版が入った」ことになる。
    let exe = dir.join("ttyskk");
    std::fs::copy(env!("CARGO_BIN_EXE_ttyskk"), &exe).expect("実行ファイルを写せない");

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");

    let mut cmd = CommandBuilder::new(&exe);
    cmd.args(["--", "sh"]);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", dir.join("no-such-config.toml"));
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

    // 行末は CR。LF (0x0a) は Ctrl+J で、かなモードへ入る合図になってしまう。
    let line = |s: &str| {
        let mut w = writer.lock().unwrap();
        w.write_all(format!("{s}\r").as_bytes()).expect("書けない");
        w.flush().expect("流せない");
    };

    std::thread::sleep(Duration::from_millis(700));
    line("stty -echo");
    std::thread::sleep(Duration::from_millis(300));

    // 子のシェルの PID を控える。差し替えをまたいで同じなら、生き残っている。
    let shell_pid = dir.join("shell.pid");
    line(&format!("echo $$ > {}", shell_pid.display()));
    let before = wait_for_file(&shell_pid, 5).expect("子のシェルの PID を取れない");

    // ここでバイナリをすげ替える。差し替え先が起きたら、申し送りを書き出し、
    // **渡された fd を実際に使ってみる**。
    //
    // 申し送りの文字列が届いただけでは足りない。あれはただの環境変数で、fd が
    // 閉じられていても素通りする。擬似端末の親側へ書けば子のシェルがそれを読んで
    // 実行するので、**fd が生きていなければ目印は現れない**。
    //
    // **消してから作る。** 上書きすると「実行中のファイルを書き換えた」ことになり、
    // 走っている側が壊れる (ETXTBSY か、混ざった中身を読む)。cargo install も
    // 別のファイルを作って置き換える。
    let through_fd = dir.join("through-fd");
    std::fs::remove_file(&exe).expect("消せない");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\n\
             printf '%s' \"$TTYSKK_HANDOVER\" > {landed}\n\
             fd=${{TTYSKK_HANDOVER%%:*}}\n\
             eval \"printf 'echo handed > {through} \\r' >&$fd\"\n\
             exec sleep 30\n",
            landed = landed.display(),
            through = through_fd.display()
        ),
    )
    .expect("差し替え先を書けない");
    std::fs::set_permissions(
        &exe,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("実行できるようにできない");

    // 包まれている側から差し替えを頼む。頼む側は**元の**実体でよい。
    line(&format!("{} --reload", env!("CARGO_BIN_EXE_ttyskk")));

    let handover = wait_for_file(&landed, 10);
    // 渡された fd が本当に生きているか。**新しい実体がそこへ書いたものを、子の
    // シェルが読んで実行できたか**で見る。
    let through = wait_for_file(&through_fd, 10);

    // 子がまだ生きているか (差し替えをまたいで同じ PID か)
    let pid: i32 = before.trim().parse().expect("PID を読めない");
    let alive = unsafe { libc::kill(pid, 0) } == 0;

    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    let out = String::from_utf8_lossy(&seen.lock().unwrap().clone()).into_owned();
    let _ = child.kill();
    let _ = child.wait();
    let _ = unsafe { libc::kill(pid, libc::SIGKILL) };

    let handover = handover.unwrap_or_else(|| {
        panic!("新しい実体が起きていない。記録: {logged:?} 画面: {out:?}");
    });
    // 申し送りは `<擬似端末の fd>:<子の PID>`
    let (fd, handed_pid) = handover
        .trim()
        .split_once(':')
        .unwrap_or_else(|| panic!("申し送りの形が違う: {handover:?}"));
    assert!(
        fd.parse::<i32>().is_ok_and(|n| n > 2),
        "擬似端末の fd が渡っていない: {handover:?}"
    );
    assert_eq!(
        handed_pid.trim(),
        before.trim(),
        "渡された子の PID が食い違う"
    );
    assert!(
        through.is_some(),
        "擬似端末の fd が新しい実体まで生きて渡っていない (番号だけ渡っても閉じていれば使えない)"
    );
    assert!(alive, "差し替えで子が道連れになった (pid {pid})");
}

/// 割り当てたキーで差し替わること。**打ちかけがあるうちは効かない**こと。
///
/// シェルへ戻らずに入れ替えたい場面 (編集器やエージェントの中) のための入口。
/// 打ちかけの最中に入れ替えると打ち込んだものが消えるので、そこは素通しする。
///
/// 「効かない」ことだけを見ると、キーがそもそも死んでいても通ってしまう。**同じ
/// キーを、取り消したあとにもう一度押して効くこと**まで見て、空回りを防ぐ。
#[test]
fn the_bound_key_hands_over_only_when_nothing_is_in_hand() {
    let dir = std::env::temp_dir().join(format!("ttyskk-handover-key-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");
    let conf = dir.join("config.toml");
    std::fs::write(&conf, "[keys]\nreload = \"C-t\"\n").expect("設定を書けない");
    let landed = dir.join("landed");
    let _ = std::fs::remove_file(&landed);

    let exe = dir.join("ttyskk");
    std::fs::copy(env!("CARGO_BIN_EXE_ttyskk"), &exe).expect("実行ファイルを写せない");

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");

    let mut cmd = CommandBuilder::new(&exe);
    cmd.args(["--", "sh"]);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", &conf);

    let mut child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);

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
    let key = |k: &[u8]| {
        let mut w = writer.lock().unwrap();
        w.write_all(k).expect("書けない");
        w.flush().expect("流せない");
    };

    std::thread::sleep(Duration::from_millis(700));

    // 差し替え先を、起きたら目印を書くだけのものにすげ替える
    std::fs::remove_file(&exe).expect("消せない");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\nprintf 'came\\n' > {}\nexec sleep 30\n",
            landed.display()
        ),
    )
    .expect("差し替え先を書けない");
    std::fs::set_permissions(
        &exe,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("実行できるようにできない");

    // 打ちかけを作る。かなモードへ入って ▽ を開く。
    key(b"\x0a"); // C-j
    std::thread::sleep(Duration::from_millis(200));
    key(b"K"); // ▽か
    std::thread::sleep(Duration::from_millis(200));

    // ここで押しても差し替わらないはず
    key(b"\x14"); // C-t
    let while_composing = wait_for_file(&landed, 2);

    // 取り消して手を空にしてから、同じキーをもう一度
    key(b"\x07"); // C-g
    std::thread::sleep(Duration::from_millis(300));
    key(b"\x14");
    let when_idle = wait_for_file(&landed, 10);

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        while_composing.is_none(),
        "▽ を抱えたまま差し替えてしまった (打ち込んだものが消える)"
    );
    assert!(
        when_idle.is_some(),
        "手が空いているのに、割り当てたキーで差し替わらない"
    );
}

/// `--status` が、包んでいる ttyskk の**いまの中身**を映すこと。
///
/// 差し替えは PID も画面も変えないので、効いたかどうかを確かめる術がここしかない。
/// 見るのは三つ。起きたときのままなら「変わらない」と言うこと、実体をすげ替えたら
/// 「入れ替わっている」と気づくこと、差し替えたあとは**また「変わらない」に戻る**
/// こと。最後のが要 — 新しい版が自分の姿を書き直していなければ、ここで気づく。
#[test]
fn the_status_follows_the_handover() {
    let dir = std::env::temp_dir().join(format!("ttyskk-status-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");
    let run = dir.join("run");
    std::fs::create_dir_all(&run).expect("控えの置き場所を作れない");

    let exe = dir.join("ttyskk");
    std::fs::copy(env!("CARGO_BIN_EXE_ttyskk"), &exe).expect("実行ファイルを写せない");

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");

    let mut cmd = CommandBuilder::new(&exe);
    cmd.args(["--", "sh"]);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", dir.join("no-such-config.toml"));
    cmd.env("XDG_RUNTIME_DIR", &run);

    let mut child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);

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
    let line = |s: &str| {
        let mut w = writer.lock().unwrap();
        w.write_all(format!("{s}\r").as_bytes()).expect("書けない");
        w.flush().expect("流せない");
    };

    std::thread::sleep(Duration::from_millis(700));
    line("stty -echo");
    std::thread::sleep(Duration::from_millis(300));

    // 尋ねる側は本物でよい。控えは PID で引くので、どの実体から呼んでも同じものを見る。
    let ask = |n: u32| {
        let out = dir.join(format!("status{n}"));
        let _ = std::fs::remove_file(&out);
        line(&format!(
            "{} --status > {} 2>&1",
            env!("CARGO_BIN_EXE_ttyskk"),
            out.display()
        ));
        wait_for_file(&out, 10)
    };

    let fresh = ask(1);

    // 実体をすげ替える。**末尾に一バイト足すだけ** — 大きさと更新時刻は変わるが、
    // ELF は末尾の余りを読まないので、そのまま起動できる。
    // 消してから作る (走っているファイルは書き換えられない)。
    let mut bytes = std::fs::read(&exe).expect("読めない");
    bytes.push(0);
    std::fs::remove_file(&exe).expect("消せない");
    std::fs::write(&exe, &bytes).expect("書けない");
    std::fs::set_permissions(
        &exe,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("実行できるようにできない");

    let stale = ask(2);

    line(&format!("{} --reload", env!("CARGO_BIN_EXE_ttyskk")));
    std::thread::sleep(Duration::from_millis(1500));

    let after = ask(3);

    let _ = child.kill();
    let _ = child.wait();

    let fresh = fresh.expect("--status が答えない");
    let stale = stale.unwrap_or_default();
    let after = after.unwrap_or_default();
    assert!(
        fresh.contains("起きたときのまま"),
        "起きた直後なのに入れ替わったと言う: {fresh:?}"
    );
    assert!(
        stale.contains("入れ替わっています"),
        "実体をすげ替えたのに気づかない: {stale:?}"
    );
    assert!(
        after.contains("起きたときのまま"),
        "差し替えたのに、新しい版が自分の姿を書き直していない: {after:?}"
    );
}

/// 差し替え先が起動できないときは、**そのまま動き続ける**。
///
/// 子を道連れにするくらいなら、差し替えは次の機会でよい。
#[test]
fn a_broken_replacement_leaves_the_wrapper_running() {
    let dir = std::env::temp_dir().join(format!("ttyskk-handover-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("一時の置き場所を作れない");
    let dict = dir.join("sys.dict");
    std::fs::write(&dict, "かんじ /漢字/\n").expect("辞書を書けない");
    let log = dir.join("trace.log");
    let _ = std::fs::remove_file(&log);

    let exe = dir.join("ttyskk");
    std::fs::copy(env!("CARGO_BIN_EXE_ttyskk"), &exe).expect("実行ファイルを写せない");

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");

    let mut cmd = CommandBuilder::new(&exe);
    cmd.args(["--", "sh"]);
    cmd.env_remove("TTYSKK_ACTIVE");
    cmd.env("TTYSKK_JISYO", &dict);
    cmd.env("TTYSKK_USER_JISYO", dir.join("user.dict"));
    cmd.env("TTYSKK_CONFIG", dir.join("no-such-config.toml"));
    cmd.env("TTYSKK_DEBUG", &log);

    let mut child = pty.slave.spawn_command(cmd).expect("ttyskk を起こせない");
    drop(pty.slave);

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

    let line = |s: &str| {
        let mut w = writer.lock().unwrap();
        w.write_all(format!("{s}\r").as_bytes()).expect("書けない");
        w.flush().expect("流せない");
    };

    std::thread::sleep(Duration::from_millis(700));
    line("stty -echo");
    std::thread::sleep(Duration::from_millis(300));

    // 差し替え先を「起動できないもの」にする (中身が空で実行ビットも無い)
    std::fs::remove_file(&exe).expect("消せない");
    std::fs::write(&exe, "").expect("書けない");

    line(&format!("{} --reload", env!("CARGO_BIN_EXE_ttyskk")));

    // 諦めた記録が出るまで待つ
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut logged = String::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        logged = std::fs::read_to_string(&log).unwrap_or_default();
        if logged.contains("差し替えられない") {
            break;
        }
    }

    // 諦めたあとも変換できるか。**動き続けていることの証拠**にする。
    let marker = dir.join("still-alive");
    line(&format!("echo ok > {}", marker.display()));
    let alive = wait_for_file(&marker, 5).is_some();

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        logged.contains("差し替えられない"),
        "差し替えを諦めた記録が無い: {logged:?}"
    );
    assert!(alive, "差し替えに失敗したあと、包むのをやめてしまった");
}
