//! 未確定文字の重ね描き。
//!
//! 子アプリの画面には一度も書き込まない。カーソル位置から先へ直接描き、消すときは
//! 画面の控えから元の内容を書き戻す。カーソル位置は控えから知るので端末に問い合わせ
//! (CSI 6n) を出さずに済み、応答が子の出力と混ざる事故が起きない。

use unicode_width::UnicodeWidthChar;

use crate::screen::Screen;
use crate::skk::{Segment, Style};

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

    /// カーソル位置から区間列を描く。画面右端では次の行へ折り返す。
    pub fn draw(&mut self, screen: &Screen, segments: &[Segment]) -> Vec<u8> {
        self.painted.clear();
        if segments.is_empty() {
            return Vec::new();
        }

        let mut out = String::new();
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
                    flush_line(&mut out, row, line_start, &line_body, col - line_start);
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
            flush_line(&mut out, row, line_start, &line_body, col - line_start);
            self.painted.push((row, line_start, col - line_start));
        }

        if out.is_empty() {
            return Vec::new();
        }
        // 描いている間はカーソルを隠してちらつきを抑える
        let mut bytes = String::from("\x1b[?25l");
        bytes.push_str(&out);
        bytes.into_bytes()
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
        let bytes = String::from_utf8(o.draw(&s, &[seg(Style::Reading, "▽かんじ")])).unwrap();
        assert!(bytes.contains("\x1b[1;3H"));
        assert!(bytes.contains("▽かんじ"));
        assert_eq!(o.painted, vec![(0, 2, 7)]);
    }

    #[test]
    fn erase_restores_original_content() {
        let s = screen_with("abcdef", 5, 20);
        let mut o = Overlay::new();
        o.draw(&s, &[seg(Style::Romaji, "xy")]);
        let bytes = String::from_utf8(o.erase(&s)).unwrap();
        assert!(bytes.contains("\x1b[1;7H"));
        assert!(o.is_empty());
    }

    #[test]
    fn wraps_at_right_edge() {
        // 幅 10、カーソルは 8 桁目。全角 3 文字は折り返す。
        let s = screen_with("12345678", 5, 10);
        let mut o = Overlay::new();
        o.draw(&s, &[seg(Style::Reading, "あいう")]);
        assert_eq!(o.painted.len(), 2);
        assert_eq!(o.painted[0], (0, 8, 2));
        assert_eq!(o.painted[1], (1, 0, 4));
    }

    #[test]
    fn truncates_at_bottom() {
        let s = screen_with("\x1b[2;9H", 2, 10);
        let mut o = Overlay::new();
        o.draw(&s, &[seg(Style::Reading, "あいうえお")]);
        // 最終行を越える分は捨てる (スクロールさせない)
        assert_eq!(o.painted.len(), 1);
        assert_eq!(o.painted[0].0, 1);
    }

    #[test]
    fn empty_preedit_draws_nothing() {
        let s = screen_with("$ ", 5, 20);
        let mut o = Overlay::new();
        assert!(o.draw(&s, &[]).is_empty());
        assert!(o.is_empty());
    }
}
