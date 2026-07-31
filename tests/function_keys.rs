//! ファンクションキーが子アプリまで素通しされることの見張り。
//!
//! **割り当てに無いキーは、来た形のまま子へ渡す。** 形を変えると子アプリが期待する
//! ものと食い違い、キーが効かなくなる。ファンクションキーは端末とプロトコルで形が
//! 何通りもあるので、そのどれもが通ることを確かめる。
//!
//! - SS3 (`ESC O P`〜`ESC O S`) … F1〜F4 の古くからの形
//! - CSI 数値 `~` (`ESC [ 1 5 ~` など) … F5 以降
//! - CSI 文字 (`ESC [ P`〜`ESC [ S`) … 拡張鍵盤プロトコル下の F1〜F4
//!
//! **`ESC [ R` (F3) はカーソル位置報告 (`CSI row ; col R`) と終端が同じ。** 位置報告を
//! 探す処理が食ってしまうと F3 だけが消える。ここが特に危ないので必ず通す。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// 拡張鍵盤プロトコルを頼み、受け取ったバイトを見える形に開いて返す子。
/// `ESC` は `^[` になる。
const CHILD: &str = "printf '\\033[>1u'; stty -echo raw; cat -v";

/// かなモードへ入る C-j (kitty 形式)。
const KANA: &[u8] = b"\x1b[106;5u";

/// 試すキーと、`cat -v` が出す姿。
const KEYS: &[(&str, &[u8], &str)] = &[
    ("F1 (SS3)", b"\x1bOP", "^[OP"),
    ("F2 (SS3)", b"\x1bOQ", "^[OQ"),
    ("F3 (SS3)", b"\x1bOR", "^[OR"),
    ("F4 (SS3)", b"\x1bOS", "^[OS"),
    ("F1 (CSI)", b"\x1b[P", "^[[P"),
    ("F2 (CSI)", b"\x1b[Q", "^[[Q"),
    ("F3 (CSI)", b"\x1b[R", "^[[R"),
    ("F4 (CSI)", b"\x1b[S", "^[[S"),
    // 拡張鍵盤プロトコルの下で実際に来る形。nvim が `CSI > 3 u` を頼むと、kitty は
    // F1〜F4 も数値の形で送る (実測: F3 が `CSI 13 ~` で届いた)。
    ("F1 (数値)", b"\x1b[11~", "^[[11~"),
    ("F2 (数値)", b"\x1b[12~", "^[[12~"),
    ("F3 (数値)", b"\x1b[13~", "^[[13~"),
    ("F4 (数値)", b"\x1b[14~", "^[[14~"),
    // 押下・離鍵の印 (`:1`) が付いた形。実測で C-h が `CSI 104;5:1u` で届いていた。
    ("S-F3 (印つき)", b"\x1b[13;2:1~", "^[[13;2:1~"),
    ("F5 (印つき)", b"\x1b[15;1:1~", "^[[15;1:1~"),
    // xterm 系の修飾つき F3。**カーソル位置報告と同じ形をしている。**
    ("S-F3 (xterm)", b"\x1b[1;2R", "^[[1;2R"),
    ("F5", b"\x1b[15~", "^[[15~"),
    ("F6", b"\x1b[17~", "^[[17~"),
    ("F7", b"\x1b[18~", "^[[18~"),
    ("F8", b"\x1b[19~", "^[[19~"),
    ("F9", b"\x1b[20~", "^[[20~"),
    ("F10", b"\x1b[21~", "^[[21~"),
    ("F11", b"\x1b[23~", "^[[23~"),
    ("F12", b"\x1b[24~", "^[[24~"),
    // 修飾付きの F3。`CSI 1 ; 5 R` は位置報告と同じ形をしている。
    ("C-F3", b"\x1b[1;5R", "^[[1;5R"),
];

#[test]
fn function_keys_reach_the_child() {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("擬似端末を開けない");

    let dir = std::env::temp_dir().join(format!("ttyskk-fkeys-{}", std::process::id()));
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
        std::thread::sleep(Duration::from_millis(160));
    };
    let take = || {
        let mut s = seen.lock().unwrap();
        let out = String::from_utf8_lossy(&s).into_owned();
        s.clear();
        out
    };

    std::thread::sleep(Duration::from_millis(600));
    take();

    // ASCII モード (何も横取りしない段) と、かなモード (打鍵を解釈する段) の両方。
    let mut missing: Vec<String> = Vec::new();
    for (mode, prelude) in [("ASCII", None), ("かな", Some(KANA))] {
        if let Some(p) = prelude {
            key(p);
            take();
        }
        for (name, bytes, want) in KEYS {
            key(bytes);
            let got = take();
            if !got.contains(want) {
                missing.push(format!("  {mode}モード {name} → 期待 {want:?} 実際 {got:?}"));
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        missing.is_empty(),
        "ファンクションキーが子へ届いていない\n{}",
        missing.join("\n")
    );
}
