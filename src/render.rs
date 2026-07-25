//! 未確定文字の重ね描き。
//!
//! 子アプリの画面には一度も書き込まない。カーソル位置から先へ直接描き、消すときは
//! 画面の控えから元の内容を書き戻す。カーソル位置は控えから知るので端末に問い合わせ
//! (CSI 6n) を出さずに済み、応答が子の出力と混ざる事故が起きない。

use unicode_width::UnicodeWidthChar;

use crate::screen::Screen;
use crate::skk::{Preedit, Segment, Style, Tint};

fn style_sgr(style: Style) -> &'static str {
    match style {
        // 変換対象は太字 + 下線
        Style::Reading => "\x1b[0;1;4m",
        // かなになっていないローマ字は緑
        Style::Romaji => "\x1b[0;1;32m",
        // 選択中の候補は太字 + 下線 + 赤
        Style::Candidate => "\x1b[0;1;4;31m",
        Style::ListItem => "\x1b[0;2m",
        Style::ListSelected => "\x1b[0;7m",
        // モードの印。反転させて色を地にすることで、小さくても目に入る。
        // カーソルの色 (OSC 12) と同じ配色にしてあるが、こちらは文字なので
        // 端末多重化器を挟んでも届く。
        Style::ModeHiragana => "\x1b[0;7;38;2;127;215;95m",
        Style::ModeKatakana => "\x1b[0;7;38;2;95;215;255m",
        Style::ModeHankaku => "\x1b[0;7;38;2;95;175;175m",
        Style::ModeZenkaku => "\x1b[0;7;38;2;215;135;255m",
    }
}

/// 描いた矩形 (行, 開始桁, 桁数)。
type Painted = (usize, usize, usize);

#[derive(Default)]
pub struct Overlay {
    painted: Vec<Painted>,
}

impl Overlay {
    pub fn new() -> Self {
        Overlay::default()
    }

    pub fn is_empty(&self) -> bool {
        self.painted.is_empty()
    }

    /// 消さずに忘れる。画面の大きさが変わって控えの座標が意味を失ったときに使う。
    pub fn forget(&mut self) {
        self.painted.clear();
    }

    /// 描いたものを控えから書き戻して消す。
    pub fn erase(&mut self, screen: &Screen) -> Vec<u8> {
        if self.painted.is_empty() {
            return Vec::new();
        }
        let mut out = String::new();
        for (row, col, len) in self.painted.drain(..) {
            out.push_str(&screen.restore_region(row, col, len));
        }
        out.into_bytes()
    }

    /// 未確定の表示を描く。
    ///
    /// カーソル位置の分と、浮かせる一覧の分をまとめて出す。
    pub fn draw(&mut self, screen: &Screen, p: &Preedit) -> Vec<u8> {
        self.painted.clear();
        let mut out = String::new();
        self.draw_tint(screen, p.cursor_tint, &mut out);
        self.draw_at_cursor(screen, &p.at_cursor, &mut out);
        self.draw_floating(screen, &p.floating, &mut out);
        if out.is_empty() {
            return Vec::new();
        }
        // 描いている間はカーソルを隠してちらつきを抑える
        let mut bytes = String::from("\x1b[?25l");
        bytes.push_str(&out);
        bytes.into_bytes()
    }

    /// カーソル位置のセルに色を敷く。
    ///
    /// 文字は控えのものをそのまま使う。書き換えるのは見た目だけなので、
    /// 下にある文字が消えたり位置がずれたりしない。カーソルが行末の空きセルに
    /// あるときは色の付いた箱に見え、カーソルそのものが色付いたように読める。
    fn draw_tint(&mut self, screen: &Screen, tint: Option<Tint>, out: &mut String) {
        let Some(t) = tint else {
            return;
        };
        let col = screen.col + t.offset;
        if col >= screen.cols {
            return;
        }
        let cell = screen.cell(screen.row, col);
        if cell.width == 0 {
            // 全角の後続セルの上には敷かない (前半を断ち切ってしまう)
            return;
        }
        // 記号を指定されていればそれを、無ければ控えの文字をそのまま出す
        let (ch, w) = match t.glyph {
            Some(g) => (g, 1),
            None => (cell.ch, (cell.width as usize).max(1)),
        };
        flush_line(
            out,
            screen.row,
            col,
            &format!("{}{}", style_sgr(t.style), ch),
            w,
        );
        self.painted.push((screen.row, col, w));
    }

    /// カーソル位置から区間列を描く。画面右端では次の行へ折り返す。
    fn draw_at_cursor(&mut self, screen: &Screen, segments: &[Segment], out: &mut String) {
        if segments.is_empty() {
            return;
        }
        let mut row = screen.row;
        let mut col = screen.col;
        // 行ごとに (開始桁, 現在桁, 本文) を組み立てる
        let mut line_start = col;
        let mut line_body = String::new();
        let mut cur_style: Option<Style> = None;
        let mut truncated = false;

        for seg in segments {
            if truncated {
                break;
            }
            for c in seg.text.chars() {
                let w = c.width().unwrap_or(0);
                if w == 0 {
                    continue;
                }
                if col + w > screen.cols {
                    // 行が尽きたのでここまでを吐き出して次の行へ
                    flush_line(out, row, line_start, &line_body, col - line_start);
                    self.painted.push((row, line_start, col - line_start));
                    line_body.clear();
                    cur_style = None;
                    row += 1;
                    col = 0;
                    line_start = 0;
                    if row >= screen.rows {
                        // 画面下端を越えるとスクロールしてしまうため、ここで打ち切る
                        truncated = true;
                        break;
                    }
                }
                if cur_style != Some(seg.style) {
                    line_body.push_str(style_sgr(seg.style));
                    cur_style = Some(seg.style);
                }
                line_body.push(c);
                col += w;
            }
        }
        if !line_body.is_empty() && row < screen.rows {
            flush_line(out, row, line_start, &line_body, col - line_start);
            self.painted.push((row, line_start, col - line_start));
        }
    }

    /// 一覧をカーソルの下の行に一行で浮かせる。最下行なら上の行へ。
    ///
    /// 下に描くと画面が流れてしまうため、行の選び方だけは譲れない
    /// (sentimental-skk も同じ理由でカーソルの上下を選び分けている)。
    /// 横幅に収まらない項目は落とす。途中で切ると読めないため。
    fn draw_floating(&mut self, screen: &Screen, segments: &[Segment], out: &mut String) {
        if segments.is_empty() {
            return;
        }
        let row = if screen.row + 1 < screen.rows {
            screen.row + 1
        } else if screen.row > 0 {
            screen.row - 1
        } else {
            return;
        };

        let mut body = String::new();
        let mut col = 0usize;
        let mut cur_style: Option<Style> = None;
        for seg in segments {
            let w: usize = seg.text.chars().filter_map(|c| c.width()).sum();
            if col + w > screen.cols {
                break;
            }
            if cur_style != Some(seg.style) {
                body.push_str(style_sgr(seg.style));
                cur_style = Some(seg.style);
            }
            body.push_str(&seg.text);
            col += w;
        }
        if col == 0 {
            return;
        }
        flush_line(out, row, 0, &body, col);
        self.painted.push((row, 0, col));
    }

    /// 描画のあとに端末の状態を子アプリのものへ戻す。
    pub fn restore_terminal(screen: &Screen) -> Vec<u8> {
        let mut out = String::new();
        out.push_str(&screen.pen.sgr());
        out.push_str(&format!("\x1b[{};{}H", screen.row + 1, screen.col + 1));
        if screen.cursor_visible {
            out.push_str("\x1b[?25h");
        } else {
            out.push_str("\x1b[?25l");
        }
        out.into_bytes()
    }
}

fn flush_line(out: &mut String, row: usize, start: usize, body: &str, _len: usize) {
    if body.is_empty() {
        return;
    }
    out.push_str(&format!("\x1b[{};{}H", row + 1, start + 1));
    out.push_str(body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use vte::Parser;

    fn screen_with(text: &str, rows: usize, cols: usize) -> Screen {
        let mut s = Screen::new(rows, cols);
        let mut p = Parser::new();
        p.advance(&mut s, text.as_bytes());
        s
    }

    /// カーソル位置に描くだけの Preedit。
    fn at(segs: &[Segment]) -> Preedit {
        Preedit {
            at_cursor: segs.to_vec(),
            ..Preedit::default()
        }
    }

    fn seg(style: Style, text: &str) -> Segment {
        Segment {
            style,
            text: text.to_string(),
        }
    }

    #[test]
    fn draws_at_cursor() {
        let s = screen_with("$ ", 5, 20);
        let mut o = Overlay::new();
        let bytes = String::from_utf8(o.draw(&s, &at(&[seg(Style::Reading, "▽かんじ")]))).unwrap();
        assert!(bytes.contains("\x1b[1;3H"));
        assert!(bytes.contains("▽かんじ"));
        assert_eq!(o.painted, vec![(0, 2, 7)]);
    }

    #[test]
    fn erase_restores_original_content() {
        let s = screen_with("abcdef", 5, 20);
        let mut o = Overlay::new();
        o.draw(&s, &at(&[seg(Style::Romaji, "xy")]));
        let bytes = String::from_utf8(o.erase(&s)).unwrap();
        assert!(bytes.contains("\x1b[1;7H"));
        assert!(o.is_empty());
    }

    #[test]
    fn wraps_at_right_edge() {
        // 幅 10、カーソルは 8 桁目。全角 3 文字は折り返す。
        let s = screen_with("12345678", 5, 10);
        let mut o = Overlay::new();
        o.draw(&s, &at(&[seg(Style::Reading, "あいう")]));
        assert_eq!(o.painted.len(), 2);
        assert_eq!(o.painted[0], (0, 8, 2));
        assert_eq!(o.painted[1], (1, 0, 4));
    }

    #[test]
    fn truncates_at_bottom() {
        let s = screen_with("\x1b[2;9H", 2, 10);
        let mut o = Overlay::new();
        o.draw(&s, &at(&[seg(Style::Reading, "あいうえお")]));
        // 最終行を越える分は捨てる (スクロールさせない)
        assert_eq!(o.painted.len(), 1);
        assert_eq!(o.painted[0].0, 1);
    }

    #[test]
    fn floating_goes_below_the_cursor() {
        let s = screen_with("$ ", 5, 20);
        let mut o = Overlay::new();
        let p = Preedit {
            at_cursor: vec![seg(Style::Candidate, "▼漢字")],
            floating: vec![seg(Style::ListItem, "a:感じ")],
            ..Preedit::default()
        };
        let bytes = String::from_utf8(o.draw(&s, &p)).unwrap();
        assert!(bytes.contains("\x1b[1;3H"), "▼ はカーソル位置");
        assert!(bytes.contains("\x1b[2;1H"), "一覧は次の行の左端");
        assert_eq!(o.painted.len(), 2);
    }

    #[test]
    fn floating_flips_above_at_the_bottom() {
        // カーソルが最下行。下に描くと画面が流れるので上に出す。
        let s = screen_with("\x1b[5;3H", 5, 20);
        let mut o = Overlay::new();
        let p = Preedit {
            at_cursor: vec![seg(Style::Candidate, "▼漢字")],
            floating: vec![seg(Style::ListItem, "a:感じ")],
            ..Preedit::default()
        };
        let bytes = String::from_utf8(o.draw(&s, &p)).unwrap();
        assert!(bytes.contains("\x1b[4;1H"), "一覧は一つ上の行");
    }

    #[test]
    fn floating_drops_what_does_not_fit() {
        let s = screen_with("$ ", 5, 12);
        let mut o = Overlay::new();
        let p = Preedit {
            at_cursor: vec![],
            floating: vec![
                seg(Style::ListItem, "a:あい"),
                seg(Style::ListItem, " "),
                seg(Style::ListItem, "s:うえおかき"),
            ],
            ..Preedit::default()
        };
        let bytes = String::from_utf8(o.draw(&s, &p)).unwrap();
        // 幅 12 に収まらない項目は落とす (途中で切ると読めない)
        assert!(bytes.contains("a:あい"));
        assert!(!bytes.contains("うえおかき"));
    }

    #[test]
    fn empty_preedit_draws_nothing() {
        let s = screen_with("$ ", 5, 20);
        let mut o = Overlay::new();
        assert!(o.draw(&s, &Preedit::default()).is_empty());
        assert!(o.is_empty());
    }
}
