//! 版にコミットを添える。
//!
//! `0.1.0` だけでは**入れ替わったのかが分からない**。cargo は git の写しを溜め込むので、
//! `cargo install --git` が古い写しを掴んでも気づけず、直したはずの不具合が直っていない、
//! という追い方の難しい状態になる。組み立てた時点のコミットを焼き込んでおけば一目で済む。
//!
//! git が無い場所では、cargo が書庫に残す `.cargo_vcs_info.json` から拾う —
//! crates.io から入れた木がこれにあたる。どちらも無ければ「不明」とし、組み立ては止めない。

use std::process::Command;

fn main() {
    // git の中身が変わったら組み立て直す。cargo の写し (`.git` がファイル) や
    // 書庫から取り出した木では見つからないので、そのときは何も頼まない。
    for p in [".git/HEAD", ".git/refs/heads"] {
        if std::path::Path::new(p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }

    let commit = git(&["rev-parse", "--short=8", "HEAD"]).or_else(vcs_info);
    // 手を入れたまま組み立てたなら、そのコミットそのものではないと分かるようにする
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());

    let stamp = match commit {
        Some(c) if dirty => format!("{c}+"),
        Some(c) => c,
        None => "コミット不明".to_string(),
    };
    println!("cargo:rustc-env=TTYSKK_COMMIT={stamp}");
}

/// cargo が書庫に残した、公開した時点のコミット。
///
/// 中身は `{"git":{"sha1":"…"},"path_in_vcs":""}`。**これを読むためだけに依存を
/// 増やさない** — 鍵の後ろの引用符の中を取り出せば足りる。
fn vcs_info() -> Option<String> {
    let text = std::fs::read_to_string(".cargo_vcs_info.json").ok()?;
    let sha = text
        .split_once("\"sha1\"")?
        .1
        .split_once('"')?
        .1
        .split_once('"')?
        .0;
    if sha.len() < 8 {
        return None;
    }
    Some(sha[..8].to_string())
}

/// git を呼び、成功したら標準出力を前後の空白を落として返す。
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
