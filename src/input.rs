//! 端末から届いたバイト列をキーに切り出す。
//!
//! エスケープ列は中身を解釈せず `Key::Raw` のまま子へ渡す。矢印キーや修飾キーの
//! 意味づけは子アプリの仕事で、入力メソッドが横取りする必要はない。

use crate::config::Config;
use crate::skk::Key;

pub struct Decoder {
    /// UTF-8 の途中で切れた分を次の読み込みまで持ち越す
    partial: Vec<u8>,
    /// SKK に割り当てられている Ctrl 付きキーの制御文字。
    ///
    /// 拡張鍵盤プロトコルで届いた形を素の制御文字に戻す対象。設定を書き換えたら
    /// `set_config` で入れ替える。
    ctrl: Vec<u8>,
}

impl Decoder {
    pub fn new(cfg: &Config) -> Self {
        Decoder {
            partial: Vec::new(),
            ctrl: cfg.ctrl_keys(),
        }
    }

    /// 設定の入れ替えに追従する。割り当てを変えたキーもその場で効くようになる。
    pub fn set_config(&mut self, cfg: &Config) {
        self.ctrl = cfg.ctrl_keys();
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Key> {
        let mut buf = std::mem::take(&mut self.partial);
        buf.extend_from_slice(data);
        let mut keys = Vec::new();
        let mut i = 0;

        while i < buf.len() {
            let b = buf[i];
            match b {
                0x1b => {
                    let (len, key) = parse_escape(&buf[i..], &self.ctrl);
                    if len == 0 {
                        // 続きが届いていない見込み。持ち越す。
                        self.partial = buf[i..].to_vec();
                        return keys;
                    }
                    keys.push(key);
                    i += len;
                }
                0x7f | 0x08 => {
                    keys.push(Key::Backspace);
                    i += 1;
                }
                0x0d => {
                    keys.push(Key::Enter);
                    i += 1;
                }
                0x09 => {
                    keys.push(Key::Tab);
                    i += 1;
                }
                0x00..=0x1f => {
                    keys.push(Key::Ctrl(b));
                    i += 1;
                }
                _ => {
                    let len = utf8_len(b);
                    if i + len > buf.len() {
                        self.partial = buf[i..].to_vec();
                        return keys;
                    }
                    match std::str::from_utf8(&buf[i..i + len]) {
                        Ok(s) => keys.push(Key::Char(s.chars().next().unwrap())),
                        // 壊れたバイトはそのまま素通しする
                        Err(_) => keys.push(Key::Raw(buf[i..i + len].to_vec())),
                    }
                    i += len;
                }
            }
        }
        keys
    }
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// エスケープ列を切り出す。長さ 0 は「まだ足りない」の意。
fn parse_escape(buf: &[u8], ctrl: &[u8]) -> (usize, Key) {
    if buf.len() == 1 {
        // 単独の ESC。続きが来ないと判断する。
        return (1, Key::Esc);
    }
    match buf[1] {
        b'[' => {
            // CSI: 引数のあと 0x40..0x7e の終端文字
            let mut i = 2;
            while i < buf.len() && !(0x40..=0x7e).contains(&buf[i]) {
                i += 1;
            }
            if i >= buf.len() {
                return (0, Key::Esc);
            }
            let seq = &buf[..i + 1];
            match decode_extended_key(seq, ctrl) {
                Some(k) => (i + 1, k),
                None => (i + 1, Key::Raw(seq.to_vec())),
            }
        }
        b'O' => {
            if buf.len() < 3 {
                return (0, Key::Esc);
            }
            (3, Key::Raw(buf[..3].to_vec()))
        }
        b']' => {
            // OSC: BEL か ST で終わる
            let mut i = 2;
            while i < buf.len() {
                if buf[i] == 0x07 {
                    return (i + 1, Key::Raw(buf[..i + 1].to_vec()));
                }
                if buf[i] == 0x1b && i + 1 < buf.len() && buf[i + 1] == b'\\' {
                    return (i + 2, Key::Raw(buf[..i + 2].to_vec()));
                }
                i += 1;
            }
            (0, Key::Esc)
        }
        // ESC + 文字 (Meta 修飾) はそのまま渡す
        _ => {
            let len = 1 + utf8_len(buf[1]);
            if len > buf.len() {
                return (0, Key::Esc);
            }
            (len, Key::Raw(buf[..len].to_vec()))
        }
    }
}

/// 端末の拡張鍵盤プロトコルで届いたキーを、素の制御文字に戻す。
///
/// Claude Code のように `CSI > 1 u` (kitty) や `CSI > 4 ; 2 m` (modifyOtherKeys) を
/// 有効にするアプリの下では、`Ctrl+J` が `0x0a` ではなく `CSI 106;5u` の形で届く。
/// このままでは SKK のモード切り替えが効かない。
///
/// 戻すのは **`ctrl` に挙がったキー、つまり設定で SKK に割り当てられているキーだけ**。
/// 他のキーを素の制御文字に直してしまうと、子アプリが期待する元の形を壊してしまう
/// ため、そのまま渡す。対象を設定から作るので、割り当てを変えても付いていく。
fn decode_extended_key(seq: &[u8], ctrl: &[u8]) -> Option<Key> {
    let last = *seq.last()?;
    if last != b'u' && last != b'~' {
        return None;
    }
    // 引数を `;` で分ける。`5:1` のような下位引数は先頭だけ見る。
    let nums: Vec<Option<u32>> = seq[2..seq.len() - 1]
        .split(|&b| b == b';')
        .map(|part| {
            let head = part.split(|&b| b == b':').next().unwrap_or(&[]);
            std::str::from_utf8(head).ok()?.parse().ok()
        })
        .collect();

    let (code, mods) = match last {
        // kitty: CSI 記号 ; 修飾 u
        b'u' => (
            nums.first().copied().flatten()?,
            nums.get(1).copied().flatten(),
        ),
        // modifyOtherKeys: CSI 27 ; 修飾 ; 記号 ~
        b'~' if nums.first().copied().flatten() == Some(27) => (
            nums.get(2).copied().flatten()?,
            nums.get(1).copied().flatten(),
        ),
        _ => return None,
    };

    // 修飾は 1 起点のビット並び (1=shift, 2=alt, 4=ctrl, 8=super)
    let has_ctrl = mods.unwrap_or(1).saturating_sub(1) & 4 != 0;
    if !has_ctrl {
        return None;
    }
    let b = ctrl_byte(code)?;
    ctrl.contains(&b).then_some(Key::Ctrl(b))
}

/// 拡張鍵盤プロトコルの記号を、Ctrl を押したときの制御文字に直す。
///
/// 端末は Ctrl を押していても記号そのもの (小文字) を送るので、こちらで畳む。
/// `C-a` = 0x01 … `C-z` = 0x1a、`C-space` = 0x00。設定が作れる Ctrl 付きキーは
/// この範囲に限られる (`config::parse_key`)。
fn ctrl_byte(code: u32) -> Option<u8> {
    match code {
        // 空白
        0x20 => Some(0x00),
        // a-z
        c @ 0x61..=0x7a => Some(c as u8 & 0x1f),
        _ => None,
    }
}

/// 入力の中からカーソル位置報告 (`CSI row ; col R`) を取り出す。
///
/// 見つかれば 0 起点の (行, 桁) と、報告を取り除いた残りのバイト列を返す。残りは
/// 利用者が先走って打った文字なので、捨てずにキーとして処理する。
pub fn take_cursor_report(buf: &[u8]) -> Option<((usize, usize), Vec<u8>)> {
    let mut from = 0;
    while let Some(off) = buf[from..].windows(2).position(|w| w == [0x1b, b'[']) {
        let start = from + off;
        if let Some((pos, len)) = parse_cursor_report(&buf[start..]) {
            let mut rest = buf[..start].to_vec();
            rest.extend_from_slice(&buf[start + len..]);
            return Some((pos, rest));
        }
        from = start + 2;
    }
    None
}

/// `CSI [?] row ; col R` を読む。戻り値は (0 起点の位置, 消費した長さ)。
fn parse_cursor_report(s: &[u8]) -> Option<((usize, usize), usize)> {
    let mut i = 2;
    if s.get(i) == Some(&b'?') {
        i += 1;
    }
    let (mut row, mut col) = (0usize, 0usize);
    let mut seen_semi = false;
    let mut digits = false;
    while i < s.len() {
        match s[i] {
            b'0'..=b'9' => {
                digits = true;
                let d = (s[i] - b'0') as usize;
                if seen_semi {
                    col = col * 10 + d;
                } else {
                    row = row * 10 + d;
                }
            }
            b';' if !seen_semi && digits => {
                seen_semi = true;
                digits = false;
            }
            b'R' if seen_semi && digits => {
                return Some(((row.saturating_sub(1), col.saturating_sub(1)), i + 1));
            }
            // 別の列だった
            _ => return None,
        }
        i += 1;
    }
    None
}

/// 子の出力がエスケープ列や UTF-8 文字の途中で切れていないかを追う。
///
/// 重ね描きは子の出力の隙間に自分のバイト列を割り込ませる。列の途中に割り込むと
/// 端末がそれを一つの壊れた列として解釈してしまうため、切れ目が安全なときだけ描く。
#[derive(Default)]
pub struct SeqTracker {
    /// 未完了の列の先頭からの持ち越し
    tail: Vec<u8>,
}

/// 持ち越しの上限。これを超える列は事実上ないので、越えたら諦めて完了扱いにする。
const MAX_TAIL: usize = 4096;

impl SeqTracker {
    pub fn new() -> Self {
        SeqTracker::default()
    }

    /// 出力を与え、末尾が「途中」なら true を返す。
    pub fn feed(&mut self, data: &[u8]) -> bool {
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(data);
        if buf.len() > MAX_TAIL {
            let cut = buf.len() - MAX_TAIL;
            buf.drain(..cut);
        }

        if let Some(i) = buf.iter().rposition(|&b| b == 0x1b)
            && !sequence_complete(&buf[i..])
        {
            self.tail = buf[i..].to_vec();
            return true;
        }
        let n = incomplete_utf8_tail(&buf);
        if n > 0 {
            self.tail = buf[buf.len() - n..].to_vec();
            return true;
        }
        false
    }
}

/// ESC で始まる列が終端まで揃っているか。
fn sequence_complete(s: &[u8]) -> bool {
    if s.len() < 2 {
        return false;
    }
    match s[1] {
        b'[' => s[2..].iter().any(|b| (0x40..=0x7e).contains(b)),
        // 文字列を伴う列は BEL か ST で終わる
        b']' | b'P' | b'X' | b'^' | b'_' => {
            s[2..].contains(&0x07) || s.windows(2).skip(2).any(|w| w == [0x1b, b'\\'])
        }
        // 中間文字を挟む列は終端文字がもう一つ要る
        b'#' | b'(' | b')' | b'*' | b'+' | b'%' | b' ' => s.len() >= 3,
        _ => true,
    }
}

/// 末尾にある未完了の UTF-8 バイト数。完結していれば 0。
fn incomplete_utf8_tail(buf: &[u8]) -> usize {
    for back in 1..=4.min(buf.len()) {
        let b = buf[buf.len() - back];
        if b < 0x80 {
            return 0;
        }
        if b >= 0xc0 {
            let need = utf8_len(b);
            return if need > back { back } else { 0 };
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 既定の割り当てで切り出す。
    fn decoder() -> Decoder {
        Decoder::new(&Config::default())
    }

    #[test]
    fn plain_ascii() {
        let mut d = decoder();
        assert_eq!(d.feed(b"ab"), vec![Key::Char('a'), Key::Char('b')]);
    }

    #[test]
    fn control_keys() {
        let mut d = decoder();
        assert_eq!(
            d.feed(b"\x0a\x0d\x7f\x09"),
            vec![Key::Ctrl(0x0a), Key::Enter, Key::Backspace, Key::Tab]
        );
    }

    #[test]
    fn arrow_key_is_raw() {
        let mut d = decoder();
        assert_eq!(d.feed(b"\x1b[A"), vec![Key::Raw(b"\x1b[A".to_vec())]);
    }

    #[test]
    fn split_escape_is_carried_over() {
        let mut d = decoder();
        assert_eq!(d.feed(b"\x1b["), vec![]);
        assert_eq!(d.feed(b"C"), vec![Key::Raw(b"\x1b[C".to_vec())]);
    }

    #[test]
    fn split_utf8_is_carried_over() {
        let mut d = decoder();
        let bytes = "あ".as_bytes();
        assert_eq!(d.feed(&bytes[..2]), vec![]);
        assert_eq!(d.feed(&bytes[2..]), vec![Key::Char('あ')]);
    }

    #[test]
    fn lone_escape() {
        let mut d = decoder();
        assert_eq!(d.feed(b"\x1b"), vec![Key::Esc]);
    }

    /// Claude Code のように kitty 鍵盤プロトコルを有効にするアプリの下でも
    /// モード切り替えが効くこと。
    #[test]
    fn decodes_kitty_control_keys() {
        let mut d = decoder();
        assert_eq!(d.feed(b"\x1b[106;5u"), vec![Key::Ctrl(0x0a)]); // Ctrl+J
        assert_eq!(d.feed(b"\x1b[103;5u"), vec![Key::Ctrl(0x07)]); // Ctrl+G
        assert_eq!(d.feed(b"\x1b[113;5u"), vec![Key::Ctrl(0x11)]); // Ctrl+Q
        // 下位引数 (イベント種別) が付いていても読める
        assert_eq!(d.feed(b"\x1b[106;5:1u"), vec![Key::Ctrl(0x0a)]);
    }

    /// 復号の対象は設定から作る。割り当てを変えたらそちらが復号される。
    #[test]
    fn follows_the_configured_bindings() {
        let cfg = Config::parse("[keys]\nhankaku_katakana = \"C-o\"\n").unwrap();
        let mut d = Decoder::new(&cfg);
        assert_eq!(d.feed(b"\x1b[111;5u"), vec![Key::Ctrl(0x0f)]); // Ctrl+O
        // 割り当てを外された Ctrl+Q は元の形のまま子へ渡る
        assert_eq!(
            d.feed(b"\x1b[113;5u"),
            vec![Key::Raw(b"\x1b[113;5u".to_vec())]
        );
        // 動いている最中の差し替えにも追従する
        d.set_config(&Config::default());
        assert_eq!(d.feed(b"\x1b[113;5u"), vec![Key::Ctrl(0x11)]);
    }

    #[test]
    fn decodes_modify_other_keys() {
        let mut d = decoder();
        assert_eq!(d.feed(b"\x1b[27;5;106~"), vec![Key::Ctrl(0x0a)]);
    }

    /// 割り当ての無いキーは元の形のまま子へ渡す。
    ///
    /// `Ctrl+C` は `ascii_keys` に入っているが、あれは子アプリのキーへの便乗なので
    /// 形を変えない。素の `0x03` に直すと、Claude Code のように `Ctrl+C` で入力欄を
    /// 空にするアプリで押した内容が消える。
    #[test]
    fn other_extended_keys_pass_through() {
        let mut d = decoder();
        for seq in [
            &b"\x1b[99;5u"[..],  // Ctrl+C (ascii_keys)
            &b"\x1b[122;5u"[..], // Ctrl+Z
            &b"\x1b[100;5u"[..], // Ctrl+D
            &b"\x1b[106;1u"[..], // 修飾なしの J
            &b"\x1b[13;5u"[..],  // Ctrl+Enter
            &b"\x1b[200~"[..],   // 括弧付き貼り付けの開始
        ] {
            assert_eq!(d.feed(seq), vec![Key::Raw(seq.to_vec())], "{seq:?}");
        }
    }

    #[test]
    fn finds_cursor_report() {
        let (pos, rest) = take_cursor_report(b"\x1b[12;34R").unwrap();
        assert_eq!(pos, (11, 33));
        assert!(rest.is_empty());
    }

    #[test]
    fn keeps_typeahead_around_the_report() {
        let (pos, rest) = take_cursor_report(b"ab\x1b[5;1Rcd").unwrap();
        assert_eq!(pos, (4, 0));
        assert_eq!(rest, b"abcd");
    }

    #[test]
    fn skips_other_sequences() {
        // 先走って押された矢印キーを報告と取り違えない
        let (pos, rest) = take_cursor_report(b"\x1b[A\x1b[7;2R").unwrap();
        assert_eq!(pos, (6, 1));
        assert_eq!(rest, b"\x1b[A");
    }

    #[test]
    fn no_report_present() {
        assert!(take_cursor_report(b"\x1b[Ahello").is_none());
        assert!(take_cursor_report(b"\x1b[12;").is_none());
    }

    #[test]
    fn tracker_detects_complete_output() {
        let mut t = SeqTracker::new();
        assert!(!t.feed(b"hello"));
        assert!(!t.feed(b"\x1b[31mred\x1b[0m"));
        assert!(!t.feed("日本語".as_bytes()));
    }

    #[test]
    fn tracker_detects_split_csi() {
        let mut t = SeqTracker::new();
        assert!(t.feed(b"abc\x1b[3"));
        assert!(!t.feed(b"1m"));
    }

    #[test]
    fn tracker_carries_over_lone_escape() {
        let mut t = SeqTracker::new();
        assert!(t.feed(b"\x1b"));
        assert!(t.feed(b"[31"));
        assert!(!t.feed(b"m"));
    }

    #[test]
    fn tracker_handles_osc() {
        let mut t = SeqTracker::new();
        assert!(t.feed(b"\x1b]0;title"));
        assert!(!t.feed(b"\x07"));
        assert!(!t.feed(b"\x1b]0;title\x1b\\"));
    }

    #[test]
    fn tracker_detects_split_utf8() {
        let mut t = SeqTracker::new();
        let bytes = "あ".as_bytes();
        assert!(t.feed(&bytes[..2]));
        assert!(!t.feed(&bytes[2..]));
    }
}
