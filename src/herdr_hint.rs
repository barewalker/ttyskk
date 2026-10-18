//! 中で動いているものを herdr に知らせる (`HERDR_AGENT`)。
//!
//! ttyskk は擬似端末をもう一枚開いて子を動かすので、**外側の端末の前景プロセス
//! グループには ttyskk しか居ない**。herdr はエージェントへ文を送る前
//! (`agent.prompt`) に「前景の処理がそのエージェントか」を確かめるため、包まれた
//! claude には何も送れなくなる (`agent ... is no longer the pane foreground
//! process`)。スマホ (Flux) からの返信もこれで止まっていた。
//!
//! herdr には包む側のための約束がある。前景の処理の環境変数に `HERDR_AGENT=<名前>`
//! があれば、その名前のエージェントとして扱う。herdr は確かめるたびに
//! `/proc/<pid>/environ` を読み直す。
//!
//! **固定で付けてはいけない。** ただのシェルまでエージェント扱いになる。そこで
//! 中の前景が変わるたびに値を書き換える。`/proc/<pid>/environ` は起動時に渡された
//! 環境変数の領域をそのまま映すので、**その領域を自分で書き換えれば外から見える値も
//! 変わる**。後から伸ばすことはできないので、起動時に空き枠 (`-` の並び) を取って
//! おき、そこへ書く。
//!
//! 空き枠のままなら herdr は知らない名前として読み飛ばす。中の前景が `zsh` や
//! `nvim` でも同じく読み飛ばされるので、名前は選り分けずにそのまま書く。

use std::ffi::{CStr, c_char};
use std::path::Path;
use std::time::{Duration, Instant};

/// herdr が見る環境変数。
pub const AGENT_ENV: &str = "HERDR_AGENT";

/// 空き枠の長さ。herdr の知っている名前は長いものでも 15 文字 (`antigravity-cli`)。
const SLOT_LEN: usize = 32;

/// 空き枠を埋める字。herdr の知っている名前には当たらない。
const FILL: u8 = b'-';

/// 空き枠の値。起動し直すときと、差し替えで次の版へ渡すときに付ける。
pub fn empty_slot() -> String {
    String::from_utf8(vec![FILL; SLOT_LEN]).unwrap()
}

/// 空き枠を取るために起こし直すべきか。
///
/// herdr のペインの中 (`HERDR_PANE_ID` がある) でだけ取る。外では誰も読まない。
/// **利用者が自分で `HERDR_AGENT` を付けていたら手を出さない** — 包む側の約束
/// どおりに使っているのだから、そちらが正しい。
pub fn wants_slot() -> bool {
    cfg!(target_os = "linux")
        && std::env::var_os("HERDR_PANE_ID").is_some()
        && std::env::var_os(AGENT_ENV).is_none()
}

unsafe extern "C" {
    static environ: *const *mut c_char;
}

/// 起動時に取った空き枠。
pub struct AgentHint {
    /// 値の書き始め (`HERDR_AGENT=` の直後)。
    value: *mut u8,
    /// いま書いてある名前。空き枠のままなら `None`。
    current: Option<String>,
    looked: Option<Instant>,
}

impl AgentHint {
    /// 出力が続いている間に見にいく間隔。[`crate::FollowCwd`] と同じ考え。
    const INTERVAL: Duration = Duration::from_millis(500);

    /// 空き枠を探す。**環境変数を書き換える前に呼ぶ** — `setenv` は値を別の場所へ
    /// 作り直すので、そこを書き換えても `/proc` からは見えない。
    pub fn find() -> Option<Self> {
        let prefix = format!("{AGENT_ENV}=");
        let mut p = unsafe { environ };
        if p.is_null() {
            return None;
        }
        loop {
            let entry = unsafe { *p };
            if entry.is_null() {
                return None;
            }
            let bytes = unsafe { CStr::from_ptr(entry) }.to_bytes();
            if let Some(value) = bytes.strip_prefix(prefix.as_bytes()) {
                if value.len() != SLOT_LEN || value.iter().any(|&b| b != FILL) {
                    return None;
                }
                return Some(AgentHint {
                    value: unsafe { entry.add(prefix.len()) }.cast(),
                    current: None,
                    looked: None,
                });
            }
            p = unsafe { p.add(1) };
        }
    }

    /// 子が何か出した。間引きに引っかからなければ見にいく。
    pub fn spoke(&mut self, master: std::os::fd::RawFd) -> Option<Option<&str>> {
        match self.looked {
            Some(last) if last.elapsed() < Self::INTERVAL => None,
            _ => self.look(master),
        }
    }

    /// 中の前景を見て、変わっていれば書き直す。書き直したときだけ新しい値を返す。
    pub fn look(&mut self, master: std::os::fd::RawFd) -> Option<Option<&str>> {
        self.looked = Some(Instant::now());
        let name = foreground_name(master).filter(|n| fits(n));
        if name == self.current {
            return None;
        }
        let bytes = encode(name.as_deref());
        // 書き換えの途中で herdr が読むと半端な名前が見えるが、知らない名前として
        // 読み飛ばされ、次に確かめたときには揃っている。
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.value, SLOT_LEN) };
        self.current = name;
        Some(self.current.as_deref())
    }
}

/// 中の端末で前景にいる処理の名前。
///
/// プロセスグループの頭の `argv[0]` の末尾 (`/home/.../claude` なら `claude`)。
/// 読めなければ `comm` (15 文字で切れる) で代える。
fn foreground_name(master: std::os::fd::RawFd) -> Option<String> {
    let pgid = unsafe { libc::tcgetpgrp(master) };
    if pgid <= 0 {
        return None;
    }
    if let Ok(cmdline) = std::fs::read(format!("/proc/{pgid}/cmdline"))
        && let Some(name) = argv0_name(&cmdline)
    {
        return Some(name);
    }
    let comm = std::fs::read_to_string(format!("/proc/{pgid}/comm")).ok()?;
    Some(comm.trim_end().to_owned()).filter(|n| !n.is_empty())
}

/// `/proc/<pid>/cmdline` の先頭の引数の末尾。
fn argv0_name(cmdline: &[u8]) -> Option<String> {
    let argv0 = cmdline.split(|&b| b == 0).next()?;
    let argv0 = std::str::from_utf8(argv0).ok()?;
    // ログインシェルは `-zsh` のように頭に `-` が付く
    let name = Path::new(argv0.trim_start_matches('-')).file_name()?.to_str()?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// 枠に収まり、環境変数の値として崩れない名前か。
fn fits(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= SLOT_LEN
        && name.bytes().all(|b| b.is_ascii_graphic())
        && !name.bytes().all(|b| b == FILL)
}

/// 枠へ書く中身。名前の後ろは NUL で埋める (herdr は NUL で区切って読むので、
/// 余りは空の行として読み飛ばされる)。名前が無ければ空き枠に戻す。
fn encode(name: Option<&str>) -> [u8; SLOT_LEN] {
    match name {
        Some(n) => {
            let mut out = [0u8; SLOT_LEN];
            out[..n.len()].copy_from_slice(n.as_bytes());
            out
        }
        None => [FILL; SLOT_LEN],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_is_the_tail_of_argv0() {
        assert_eq!(
            argv0_name(b"/home/u/.local/bin/claude\0--resume\0abc\0").as_deref(),
            Some("claude")
        );
        assert_eq!(argv0_name(b"-zsh\0").as_deref(), Some("zsh"));
        assert_eq!(argv0_name(b"\0"), None);
    }

    #[test]
    fn a_name_is_written_then_padded_with_nul() {
        let out = encode(Some("claude"));
        assert_eq!(&out[..6], b"claude");
        assert!(out[6..].iter().all(|&b| b == 0));
        assert_eq!(encode(None), [FILL; SLOT_LEN]);
    }

    #[test]
    fn names_that_break_the_slot_are_refused() {
        assert!(fits("claude"));
        assert!(fits("antigravity-cli"));
        assert!(!fits(""));
        assert!(!fits("a b"));
        assert!(!fits(&"x".repeat(SLOT_LEN + 1)));
        // 空き枠そのものと見分けがつかなくなる
        assert!(!fits("---"));
    }
}
