//! 空の環境変数は「設定されていない」と同じに扱う。
//!
//! XDG の決まりでは空の変数は未設定と同じ意味になる。fcitx5 の addon (C++ 側の
//! `envOr`) は最初からそう読んでいるので、端末側だけ素直に受け取ると、**同じ環境
//! なのに端末と GUI で設定や辞書の置き場所が食い違う**。
//!
//! 置き場所は `-h` の「いま読んでいるもの」に出るので、それを突き合わせて確かめる。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 置き場所を決める環境変数。試験のたびに全部落としてから、要るものだけ入れ直す。
const PLACE_VARS: &[&str] = &[
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "TTYSKK_CONFIG",
    "TTYSKK_USER_JISYO",
    "TTYSKK_JISYO",
];

/// `-h` の「いま読んでいるもの」を (名前 → パス) にほどく。
///
/// 同じ名前が並ぶ共有辞書は最初の一つだけ見る (ここで確かめたいのはホーム由来の分)。
fn places(envs: &[(&str, &str)], home: &Path) -> BTreeMap<String, String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ttyskk"));
    cmd.arg("-h").env("HOME", home);
    for k in PLACE_VARS {
        cmd.env_remove(k);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("ttyskk -h を起こせた");
    assert!(out.status.success(), "-h が失敗した");
    let text = String::from_utf8(out.stdout).expect("使い方は UTF-8");

    let mut map = BTreeMap::new();
    for line in text
        .lines()
        .skip_while(|l| !l.starts_with("いま読んでいるもの"))
        .skip(1)
        .take_while(|l| l.starts_with("    "))
    {
        let mut it = line.split_whitespace();
        let (Some(name), Some(path)) = (it.next(), it.next()) else {
            continue;
        };
        map.entry(name.to_string())
            .or_insert_with(|| path.to_string());
    }
    assert!(!map.is_empty(), "置き場所を読み取れた:\n{text}");
    map
}

fn home_for(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ttyskk-env-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 空の `XDG_*` は、設定されていないのと同じ置き場所になる。
#[test]
fn an_empty_xdg_variable_falls_back_to_the_home() {
    let home = home_for("xdg");
    let unset = places(&[], &home);
    let empty = places(&[("XDG_CONFIG_HOME", ""), ("XDG_DATA_HOME", "")], &home);
    assert_eq!(unset, empty, "空の XDG_* が未設定と食い違う");

    // 念のため、ホーム由来の場所になっていること (相対パスに落ちていない)
    assert_eq!(
        unset.get("設定").map(String::as_str),
        Some(home.join(".config/ttyskk/config.toml").to_str().unwrap())
    );
    assert_eq!(
        unset.get("利用者辞書").map(String::as_str),
        Some(home.join(".local/share/ttyskk/user.dict").to_str().unwrap())
    );
}

/// 値が入っていれば、もちろんそちらを使う (この試験が空回りしていないことの確認)。
#[test]
fn a_filled_xdg_variable_is_honoured() {
    let home = home_for("filled");
    let filled = places(&[("XDG_CONFIG_HOME", "/tmp/somewhere")], &home);
    assert_eq!(
        filled.get("設定").map(String::as_str),
        Some("/tmp/somewhere/ttyskk/config.toml")
    );
}

/// 空の `TTYSKK_*` も同じ。置き場所を指していないのだから、既定へ回る。
#[test]
fn an_empty_ttyskk_variable_falls_back_too() {
    let home = home_for("ttyskk");
    let unset = places(&[], &home);
    let empty = places(&[("TTYSKK_CONFIG", ""), ("TTYSKK_USER_JISYO", "")], &home);
    assert_eq!(unset, empty, "空の TTYSKK_* が未設定と食い違う");
}
