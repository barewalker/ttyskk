//! 端末から届いたバイト列をキーに切り出す。
//!
//! エスケープ列は中身を解釈せず `Key::Raw` のまま子へ渡す。矢印キーや修飾キーの
//! 意味づけは子アプリの仕事で、入力メソッドが横取りする必要はない。

use ttyskk::config::Config;
use ttyskk::skk::{Key, PASTE_END, PASTE_START, SHIFT_TAB};

/// 貼り付けを溜め込む上限。これを超えたら諦めて素通しに切り替える。
///
/// 端末が終了の列を送り損ねた場合や、貼り付けた中身にたまたま開始の列が含まれて
/// いた場合に、際限なく溜め続けないための保険。
const MAX_PASTE: usize = 1 << 20;

pub struct Decoder {
    /// UTF-8 の途中で切れた分を次の読み込みまで持ち越す
    partial: Vec<u8>,
    /// SKK に割り当てられているキー。
    ///
    /// 拡張鍵盤プロトコルで届いた形を素の形に戻す対象。設定を書き換えたら
    /// `set_config` で入れ替える。
    bound: Vec<Key>,
    /// 括弧付き貼り付けの最中なら、そこまでに溜めた中身。
    pasting: Option<Vec<u8>>,
}

impl Decoder {
    pub fn new(cfg: &Config) -> Self {
        Decoder {
            partial: Vec::new(),
            bound: cfg.bound_keys(),
            pasting: None,
        }
    }

    /// 設定の入れ替えに追従する。割り当てを変えたキーもその場で効くようになる。
    pub fn set_config(&mut self, cfg: &Config) {
        self.bound = cfg.bound_keys();
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Key> {
        let mut buf = std::mem::take(&mut self.partial);
        buf.extend_from_slice(data);
        let mut keys = Vec::new();
        let mut i = 0;

        while i < buf.len() {
            // 貼り付けの最中は中身を一切解釈せず、終わりの列まで溜める
            if let Some(acc) = self.pasting.as_mut() {
                match find(&buf[i..], PASTE_END) {
                    Some(at) => {
                        acc.extend_from_slice(&buf[i..i + at]);
                        keys.push(Key::Paste(std::mem::take(acc)));
                        self.pasting = None;
                        i += at + PASTE_END.len();
                    }
                    None => {
                        // 終わりの列が読み込みの境界で切れていることがある。
                        // その手前までを溜め、切れかけの分だけ持ち越す。
                        let keep = trailing_prefix(&buf[i..], PASTE_END);
                        let end = buf.len() - keep;
                        acc.extend_from_slice(&buf[i..end]);
                        if acc.len() > MAX_PASTE {
                            // 終わりが来ない。溜めた分を囲みごとそのまま子へ流す。
                            let mut raw = PASTE_START.to_vec();
                            raw.append(acc);
                            keys.push(Key::Raw(raw));
                            self.pasting = None;
                        }
                        self.partial = buf[end..].to_vec();
                        return keys;
                    }
                }
                continue;
            }
            let b = buf[i];
            match b {
                0x1b => {
                    let (len, key) = parse_escape(&buf[i..], &self.bound);
                    if len == 0 {
                        // 続きが届いていない見込み。持ち越す。
                        self.partial = buf[i..].to_vec();
                        return keys;
                    }
                    if key == Key::Raw(PASTE_START.to_vec()) {
                        self.pasting = Some(Vec::new());
                        i += len;
                        continue;
                    }
                    keys.push(key);
                    i += len;
                }
                // `0x08` (C-h) はここでは制御キーのまま通す。backspace として扱うかは
                // 設定 (`keys.backspace`) が決めることで、切り出す側の決め打ちにしない。
                0x7f => {
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

/// `pat` が最初に現れる位置。
fn find(buf: &[u8], pat: &[u8]) -> Option<usize> {
    buf.windows(pat.len()).position(|w| w == pat)
}

/// 末尾にある `pat` の前半部分の長さ。次の読み込みへ持ち越す分。
fn trailing_prefix(buf: &[u8], pat: &[u8]) -> usize {
    let max = (pat.len() - 1).min(buf.len());
    (1..=max)
        .rev()
        .find(|&n| buf[buf.len() - n..] == pat[..n])
        .unwrap_or(0)
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
fn parse_escape(buf: &[u8], bound: &[Key]) -> (usize, Key) {
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
            if seq == SHIFT_TAB {
                return (i + 1, Key::ShiftTab);
            }
            match decode_extended_key(seq, bound) {
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

/// 端末の拡張鍵盤プロトコルで届いたキーを、素の形に戻す。
///
/// Claude Code のように `CSI > 1 u` (kitty) や `CSI > 4 ; 2 m` (modifyOtherKeys) を
/// 有効にするアプリの下では、`Ctrl+J` が `0x0a` ではなく `CSI 106;5u` の形で届く。
/// このままでは SKK のモード切り替えが効かない。
///
/// 戻すのは **`bound` に挙がったキー、つまり設定で SKK に割り当てられているキーだけ**。
/// 他のキーを素の形に直してしまうと、子アプリが期待する元の形を壊してしまうため、
/// そのまま渡す。対象を設定から作るので、割り当てを変えても付いていく。
fn decode_extended_key(seq: &[u8], bound: &[Key]) -> Option<Key> {
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

    let k = named_key(code, mods.unwrap_or(1))?;
    bound.contains(&k).then_some(k)
}

/// 拡張鍵盤プロトコルの (記号, 修飾) を、素の形のキーに直す。
///
/// 端末は修飾キーを押していても記号そのもの (小文字) を送るので、こちらで畳む。
/// `C-a` = 0x01 … `C-z` = 0x1a、`C-space` = 0x00、`Shift+Tab` は `CSI Z` に相当。
/// 設定が作れる修飾付きのキーはこれだけ (`config::parse_key`)。
///
/// 修飾の無い打鍵は対象外 — 印字文字は「曖昧さの解消」段階では素のまま届くので、
/// 戻す必要がない。
fn named_key(code: u32, mods: u32) -> Option<Key> {
    // 修飾は 1 起点のビット並び (1=shift, 2=alt, 4=ctrl, 8=super)
    let m = mods.saturating_sub(1);
    let (shift, ctrl) = (m & 1 != 0, m & 4 != 0);
    match code {
        // 空白
        0x20 if ctrl => Some(Key::Ctrl(0x00)),
        // a-z
        c @ 0x61..=0x7a if ctrl => Some(Key::Ctrl(c as u8 & 0x1f)),
        // @ \ ] ^ _ — Ctrl は記号にも付く (`C-]` = 0x1d)。
        // `[` は入れない。Ctrl+[ は Esc そのもので、設定でも書けないようにしてある。
        c @ (0x40 | 0x5c..=0x5f) if ctrl => Some(Key::Ctrl(c as u8 & 0x1f)),
        // Tab
        0x09 if shift && !ctrl => Some(Key::ShiftTab),
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

    /// 記号に付いた Ctrl も、拡張鍵盤プロトコルの下で素の形へ戻ること。
    ///
    /// `C-]` は「子アプリから奪っても痛くないキー」として `keys.reload` の置き場に
    /// なる。奪う相手が Claude Code なのだから、その下で効かなければ意味が無い。
    #[test]
    fn decodes_kitty_control_symbols() {
        let cfg = Config::parse("[keys]\nreload = \"C-]\"\n").unwrap();
        let mut d = Decoder::new(&cfg);
        assert_eq!(d.feed(b"\x1b[93;5u"), vec![Key::Ctrl(0x1d)]); // Ctrl+]
        // 素の端末では制御文字そのものが届く
        assert_eq!(d.feed(b"\x1d"), vec![Key::Ctrl(0x1d)]);
        // 割り当てが無ければ元のバイト列のまま子へ渡す
        let mut plain = decoder();
        assert_ne!(plain.feed(b"\x1b[93;5u"), vec![Key::Ctrl(0x1d)]);
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

    /// C-h は制御キーのまま切り出す。backspace として扱うかは設定が決める。
    ///
    /// ここを `Key::Backspace` に潰していると、押されたのが `0x08` だったのか
    /// `0x7f` だったのかが分からなくなる。
    #[test]
    fn ctrl_h_stays_a_control_key() {
        let mut d = decoder();
        assert_eq!(d.feed(b"\x08"), vec![Key::Ctrl(0x08)]);
        assert_eq!(d.feed(b"\x7f"), vec![Key::Backspace]);
    }

    /// **拡張鍵盤プロトコルの下でも C-h が届くこと。**
    ///
    /// `keys.backspace` に挙がっていないと `CSI 104;5u` が素通しされ、▽ の途中で
    /// 押したときに見出し語がそこで確定してしまう (消すどころか出てしまう)。
    #[test]
    fn decodes_kitty_ctrl_h() {
        let mut d = decoder();
        assert_eq!(d.feed(b"\x1b[104;5u"), vec![Key::Ctrl(0x08)]);
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
            &b"\x1b[119;5u"[..], // Ctrl+W
            &b"\x1b[106;1u"[..], // 修飾なしの J
            &b"\x1b[13;5u"[..],  // Ctrl+Enter
        ] {
            assert_eq!(d.feed(seq), vec![Key::Raw(seq.to_vec())], "{seq:?}");
        }
    }

    /// Shift+Tab は名前のあるキーとして切り出す (端末は `CSI Z` として送る)。
    ///
    /// 設定に端末のバイト列が現れないので、GUI の入力メソッドからも同じキーを作れる。
    #[test]
    fn shift_tab_is_a_named_key() {
        let mut d = decoder();
        assert_eq!(d.feed(SHIFT_TAB), vec![Key::ShiftTab]);
        // 拡張鍵盤プロトコルの下でも同じキーになる (Ctrl 付きと同じ扱い)
        assert_eq!(d.feed(b"\x1b[9;2u"), vec![Key::ShiftTab]);
        assert_eq!(d.feed(b"\x1b[27;2;9~"), vec![Key::ShiftTab]);
        // 修飾なしの Tab は素の 0x09 で届くので、こちらは Tab のまま
        assert_eq!(d.feed(b"\x09"), vec![Key::Tab]);
    }

    /// 割り当てが外れていれば Shift+Tab も復号しない。
    #[test]
    fn shift_tab_follows_the_bindings() {
        let cfg = Config::parse("[keys]\ncomplete_previous = \"C-p\"\n").unwrap();
        let mut d = Decoder::new(&cfg);
        assert_eq!(d.feed(b"\x1b[9;2u"), vec![Key::Raw(b"\x1b[9;2u".to_vec())]);
        assert_eq!(d.feed(b"\x1b[112;5u"), vec![Key::Ctrl(0x10)]);
        // 素の CSI Z は端末の Shift+Tab そのものなので、割り当てに関わらず名前が付く
        assert_eq!(d.feed(SHIFT_TAB), vec![Key::ShiftTab]);
    }

    /// 貼り付けは打鍵ではないので、中身を一つのキーにまとめる。
    #[test]
    fn bracketed_paste_becomes_one_key() {
        let mut d = decoder();
        assert_eq!(
            d.feed(b"\x1b[200~hello, world\x1b[201~"),
            vec![Key::Paste(b"hello, world".to_vec())]
        );
    }

    /// 中身は一切解釈しない。矢印でもモード切り替えのキーでも、そのまま溜める。
    #[test]
    fn paste_content_is_never_interpreted() {
        let mut d = decoder();
        assert_eq!(
            d.feed(b"\x1b[200~l\x1b[A\x0aq\x1b[201~"),
            vec![Key::Paste(b"l\x1b[A\x0aq".to_vec())]
        );
    }

    /// 大きな貼り付けは読み込みの境界で切れる。囲みの列そのものが切れることもある。
    #[test]
    fn split_paste_is_carried_over() {
        let mut d = decoder();
        assert_eq!(d.feed(b"\x1b[200~hel"), vec![]);
        assert_eq!(d.feed(b"lo"), vec![]);
        // 終わりの列の途中で切れても、前半を中身と取り違えない
        assert_eq!(d.feed(b"\x1b[20"), vec![]);
        assert_eq!(d.feed(b"1~"), vec![Key::Paste(b"hello".to_vec())]);

        // 開始の列が切れる場合も同じ
        assert_eq!(d.feed(b"\x1b[2"), vec![]);
        assert_eq!(d.feed(b"00~ab\x1b[201~"), vec![Key::Paste(b"ab".to_vec())]);
    }

    /// 日本語を含む長い貼り付けが読み込みの境界で切れても、中身は一バイトも変わらない。
    ///
    /// 標準入力は 4096 バイトずつ読む。かなは 3 バイトなので境界はほぼ必ず文字の
    /// 途中に落ちる。そこで一バイトでも落とすと、受け取った側で置換文字 (U+FFFD)
    /// になる。**あらゆる境界で切って**中身が変わらないことを見張る。
    #[test]
    fn split_paste_keeps_multibyte_bytes_intact() {
        let body = "ただ一点、私の計算違いでしたら申し訳ないのですが、".repeat(60);
        let want = vec![Key::Paste(body.as_bytes().to_vec())];
        let mut input = PASTE_START.to_vec();
        input.extend_from_slice(body.as_bytes());
        input.extend_from_slice(PASTE_END);

        // 実際の読み込み単位で切る
        let mut d = decoder();
        let mut keys = Vec::new();
        for chunk in input.chunks(4096) {
            keys.extend(d.feed(chunk));
        }
        assert_eq!(keys, want, "4096 バイトずつ");

        // 境界を一バイトずつずらして二つに切る。囲みの列そのものが切れる場合は
        // split_paste_is_carried_over が見ているので、中身の途中だけを試す。
        for at in PASTE_START.len()..input.len() - PASTE_END.len() {
            let mut d = decoder();
            let mut keys = d.feed(&input[..at]);
            keys.extend(d.feed(&input[at..]));
            assert!(
                keys == want,
                "境界 {at} で中身が変わった: キー {} 個",
                keys.len()
            );
        }
    }

    /// 貼り付けの前後に打鍵が続いていても取りこぼさない。
    #[test]
    fn keys_around_a_paste_survive() {
        let mut d = decoder();
        assert_eq!(
            d.feed(b"a\x1b[200~xy\x1b[201~b"),
            vec![Key::Char('a'), Key::Paste(b"xy".to_vec()), Key::Char('b'),]
        );
    }

    /// 終わりの列が来ないまま溜まり続けたら、囲みごと素通しに切り替える。
    #[test]
    fn a_paste_without_an_end_gives_up() {
        let mut d = decoder();
        let big = vec![b'x'; MAX_PASTE + 1];
        let mut input = PASTE_START.to_vec();
        input.extend_from_slice(&big);
        let keys = d.feed(&input);
        assert_eq!(keys.len(), 1);
        match &keys[0] {
            Key::Raw(v) => {
                assert!(v.starts_with(PASTE_START));
                assert_eq!(v.len(), PASTE_START.len() + big.len());
            }
            other => panic!("素通しになるはず: {other:?}"),
        }
        // 諦めたあとは普通の打鍵に戻る
        assert_eq!(d.feed(b"a"), vec![Key::Char('a')]);
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
