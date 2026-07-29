//! 版にコミットを添える。
//!
//! `0.1.0` だけでは**入れ替わったのかが分からない**。cargo は git の写しを溜め込むので、
//! `cargo install --git` が古い写しを掴んでも気づけず、直したはずの不具合が直っていない、
//! という追い方の難しい状態になる。組み立てた時点のコミットを焼き込んでおけば一目で済む。
//!
//! git が無い場所や、書庫から取り出しただけの木では「不明」になる。組み立ては止めない。

use std::process::Command;

fn main() {
    // git の中身が変わったら組み立て直す。cargo の写し (`.git` がファイル) や
    // 書庫から取り出した木では見つからないので、そのときは何も頼まない。
    for p in [".git/HEAD", ".git/refs/heads"] {
        if std::path::Path::new(p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }

    let commit = git(&["rev-parse", "--short=8", "HEAD"]);
    // 手を入れたまま組み立てたなら、そのコミットそのものではないと分かるようにする
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());

    let stamp = match commit {
        Some(c) if dirty => format!("{c}+"),
        Some(c) => c,
        None => "コミット不明".to_string(),
    };
    println!("cargo:rustc-env=TTYSKK_COMMIT={stamp}");
}

/// git を呼び、成功したら標準出力を前後の空白を落として返す。
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
