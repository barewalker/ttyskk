//! 子プロセスの出力を追って画面の控えを保つ。
//!
//! 重ね描きした未確定文字を消すには「そこに元々何があったか」を知る必要がある。
//! 端末に問い合わせる (CSI 6n など) と応答が子の出力と混ざるため、出力を横から
//! 読んで格子に写し取っておく。完全な端末模倣は不要で、文字と表示属性、そして
//! カーソル位置が正しく追えれば足りる。

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub const BOLD: u8 = 1 << 0;
pub const DIM: u8 = 1 << 1;
pub const ITALIC: u8 = 1 << 2;
pub const UNDERLINE: u8 = 1 << 3;
pub const BLINK: u8 = 1 << 4;
pub const REVERSE: u8 = 1 << 5;
pub const HIDDEN: u8 = 1 << 6;
pub const STRIKE: u8 = 1 << 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Attr {
    pub fg: Color,
    pub bg: Color,
    pub flags: u8,
}

impl Attr {
    /// この属性を再現する SGR 列。前の状態に依らないよう必ず 0 で始める。
    pub fn sgr(&self) -> String {
        let mut s = String::from("\x1b[0");
        for (flag, code) in [
            (BOLD, "1"),
            (DIM, "2"),
            (ITALIC, "3"),
            (UNDERLINE, "4"),
            (BLINK, "5"),
            (REVERSE, "7"),
            (HIDDEN, "8"),
            (STRIKE, "9"),
        ] {
            if self.flags & flag != 0 {
                s.push(';');
                s.push_str(code);
            }
        }
        match self.fg {
            Color::Default => {}
            Color::Indexed(i) if i < 8 => s.push_str(&format!(";{}", 30 + i)),
            Color::Indexed(i) if i < 16 => s.push_str(&format!(";{}", 90 + i - 8)),
            Color::Indexed(i) => s.push_str(&format!(";38;5;{}", i)),
            Color::Rgb(r, g, b) => s.push_str(&format!(";38;2;{};{};{}", r, g, b)),
        }
        match self.bg {
            Color::Default => {}
            Color::Indexed(i) if i < 8 => s.push_str(&format!(";{}", 40 + i)),
            Color::Indexed(i) if i < 16 => s.push_str(&format!(";{}", 100 + i - 8)),
            Color::Indexed(i) => s.push_str(&format!(";48;5;{}", i)),
            Color::Rgb(r, g, b) => s.push_str(&format!(";48;2;{};{};{}", r, g, b)),
        }
        s.push('m');
        s
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    pub ch: char,
    pub attr: Attr,
    /// 表示幅。全角の後続セルは 0。
    pub width: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            attr: Attr::default(),
            width: 1,
        }
    }
}

type Grid = Vec<Vec<Cell>>;

fn new_grid(rows: usize, cols: usize) -> Grid {
    vec![vec![Cell::default(); cols]; rows]
}

pub struct Screen {
    pub rows: usize,
    pub cols: usize,
    grid: Grid,
    /// 副画面 (DECSET 1049) に切り替わっている間、主画面を退避しておく
    saved_grid: Option<Grid>,
    pub row: usize,
    pub col: usize,
    /// 右端に達したが折り返しはまだ、という宙ぶらりんの状態
    wrap_pending: bool,
    pub pen: Attr,
    saved_cursor: (usize, usize, Attr),
    scroll_top: usize,
    scroll_bot: usize,
    autowrap: bool,
    pub cursor_visible: bool,
    pub alt_screen: bool,
    /// 子アプリがカーソルの形 (DECSCUSR) や色 (OSC 12/112) を変えたか。
    /// モードの合図を上書きされたことになるので、呼び出し側が塗り直す。
    cursor_style_touched: bool,
}

impl Screen {
    pub fn new(rows: usize, cols: usize) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        Screen {
            rows,
            cols,
            grid: new_grid(rows, cols),
            saved_grid: None,
            row: 0,
            col: 0,
            wrap_pending: false,
            pen: Attr::default(),
            saved_cursor: (0, 0, Attr::default()),
            scroll_top: 0,
            scroll_bot: rows - 1,
            autowrap: true,
            cursor_visible: true,
            alt_screen: false,
            cursor_style_touched: false,
        }
    }

    /// 子アプリがカーソルの見た目を変えていたら true を返し、印を落とす。
    pub fn take_cursor_style_touched(&mut self) -> bool {
        std::mem::take(&mut self.cursor_style_touched)
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        self.rows = rows;
        self.cols = cols;
        self.grid = new_grid(rows, cols);
        self.saved_grid = self.saved_grid.as_ref().map(|_| new_grid(rows, cols));
        self.row = self.row.min(rows - 1);
        self.col = self.col.min(cols - 1);
        self.scroll_top = 0;
        self.scroll_bot = rows - 1;
        self.wrap_pending = false;
    }

    /// カーソル位置を外から合わせる。端末に尋ねた実際の位置を基準にするために使う。
    pub fn set_cursor(&mut self, row: usize, col: usize) {
        self.row = row.min(self.rows - 1);
        self.col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    pub fn cell(&self, row: usize, col: usize) -> Cell {
        self.grid
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or_default()
    }

    /// 控えから [col, col+len) の内容を書き戻す escape 列を作る。
    /// 全角文字を途中で断ち切らないよう範囲は自動で広げる。
    pub fn restore_region(&self, row: usize, col: usize, len: usize) -> String {
        if row >= self.rows || len == 0 {
            return String::new();
        }
        let mut start = col.min(self.cols);
        let mut end = (col + len).min(self.cols);
        // 全角の後続セル (width 0) から始まるなら先頭側へ寄せる
        while start > 0 && self.cell(row, start).width == 0 {
            start -= 1;
        }
        // 末尾が全角の前半で切れるなら後ろへ伸ばす
        while end < self.cols && self.cell(row, end).width == 0 {
            end += 1;
        }
        if start >= end {
            return String::new();
        }

        let mut out = format!("\x1b[{};{}H", row + 1, start + 1);
        let mut cur: Option<Attr> = None;
        let mut c = start;
        while c < end {
            let cell = self.cell(row, c);
            if cell.width == 0 {
                c += 1;
                continue;
            }
            if cur != Some(cell.attr) {
                out.push_str(&cell.attr.sgr());
                cur = Some(cell.attr);
            }
            out.push(cell.ch);
            c += (cell.width as usize).max(1);
        }
        out
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n {
            if self.scroll_top <= self.scroll_bot && self.scroll_bot < self.rows {
                self.grid.remove(self.scroll_top);
                self.grid
                    .insert(self.scroll_bot, vec![Cell::default(); self.cols]);
            }
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n {
            if self.scroll_top <= self.scroll_bot && self.scroll_bot < self.rows {
                self.grid.remove(self.scroll_bot);
                self.grid
                    .insert(self.scroll_top, vec![Cell::default(); self.cols]);
            }
        }
    }

    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.row == self.scroll_bot {
            self.scroll_up(1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
    }

    fn clear_cells(&mut self, row: usize, from: usize, to: usize) {
        let attr = Attr {
            // 消去した箇所は背景色だけ引き継ぐのが一般的な端末の挙動
            bg: self.pen.bg,
            ..Attr::default()
        };
        if let Some(r) = self.grid.get_mut(row) {
            for c in from..to.min(r.len()) {
                r[c] = Cell {
                    ch: ' ',
                    attr,
                    width: 1,
                };
            }
        }
    }

    fn set_sgr(&mut self, params: &Params) {
        let items: Vec<Vec<u16>> = params.iter().map(|p| p.to_vec()).collect();
        if items.is_empty() {
            self.pen = Attr::default();
            return;
        }
        let mut i = 0;
        while i < items.len() {
            let sub = &items[i];
            let p = sub.first().copied().unwrap_or(0);
            match p {
                0 => self.pen = Attr::default(),
                1 => self.pen.flags |= BOLD,
                2 => self.pen.flags |= DIM,
                3 => self.pen.flags |= ITALIC,
                4 => self.pen.flags |= UNDERLINE,
                5 => self.pen.flags |= BLINK,
                7 => self.pen.flags |= REVERSE,
                8 => self.pen.flags |= HIDDEN,
                9 => self.pen.flags |= STRIKE,
                21 | 22 => self.pen.flags &= !(BOLD | DIM),
                23 => self.pen.flags &= !ITALIC,
                24 => self.pen.flags &= !UNDERLINE,
                25 => self.pen.flags &= !BLINK,
                27 => self.pen.flags &= !REVERSE,
                28 => self.pen.flags &= !HIDDEN,
                29 => self.pen.flags &= !STRIKE,
                30..=37 => self.pen.fg = Color::Indexed((p - 30) as u8),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((p - 40) as u8),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((p - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((p - 100 + 8) as u8),
                38 | 48 => {
                    // 38;5;n / 38;2;r;g;b は「;」区切りと「:」区切りの両方があり得る
                    let (color, consumed) = if sub.len() > 1 {
                        (parse_ext_color(&sub[1..]), 0)
                    } else {
                        let rest: Vec<u16> = items[i + 1..]
                            .iter()
                            .filter_map(|s| s.first().copied())
                            .collect();
                        let n = match rest.first() {
                            Some(5) => 2,
                            Some(2) => 4,
                            _ => 0,
                        };
                        (parse_ext_color(&rest), n)
                    };
                    if let Some(c) = color {
                        if p == 38 {
                            self.pen.fg = c;
                        } else {
                            self.pen.bg = c;
                        }
                    }
                    i += consumed;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

fn parse_ext_color(v: &[u16]) -> Option<Color> {
    match v.first()? {
        5 => Some(Color::Indexed(*v.get(1)? as u8)),
        2 => {
            // 「:」区切りでは色空間 ID が挟まることがある
            let base = if v.len() >= 5 { 2 } else { 1 };
            Some(Color::Rgb(
                *v.get(base)? as u8,
                *v.get(base + 1)? as u8,
                *v.get(base + 2)? as u8,
            ))
        }
        _ => None,
    }
}

fn arg(params: &Params, idx: usize, default: u16) -> usize {
    let v = params
        .iter()
        .nth(idx)
        .and_then(|p| p.first().copied())
        .unwrap_or(0);
    if v == 0 { default as usize } else { v as usize }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        let w = c.width().unwrap_or(0);
        if w == 0 {
            return;
        }
        if self.wrap_pending || self.col + w > self.cols {
            if self.autowrap {
                self.col = 0;
                self.linefeed();
            } else {
                self.col = self.cols.saturating_sub(w);
            }
        }
        self.wrap_pending = false;
        let (row, col, cols) = (self.row, self.col, self.cols);
        let attr = self.pen;
        if let Some(r) = self.grid.get_mut(row)
            && col < r.len()
        {
            r[col] = Cell {
                ch: c,
                attr,
                width: w as u8,
            };
            for k in 1..w {
                if col + k < r.len() {
                    r[col + k] = Cell {
                        ch: ' ',
                        attr,
                        width: 0,
                    };
                }
            }
        }
        self.col += w;
        if self.col >= cols {
            self.col = cols - 1;
            self.wrap_pending = true;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => {
                // BS
                self.wrap_pending = false;
                self.col = self.col.saturating_sub(1);
            }
            0x09 => {
                // HT
                self.wrap_pending = false;
                self.col = ((self.col / 8) + 1) * 8;
                if self.col >= self.cols {
                    self.col = self.cols - 1;
                }
            }
            0x0a..=0x0c => self.linefeed(),
            0x0d => {
                self.wrap_pending = false;
                self.col = 0;
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 12 (カーソル色の設定) と OSC 112 (既定へ戻す)。DECSCUSR と同じ理由で
        // 印を付ける。それ以外の OSC (題名など) は素通しでよい。
        if let Some(first) = params.first()
            && matches!(*first, b"12" | b"112")
        {
            self.cursor_style_touched = true;
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // 私用マーカー (`?` `>` `<` `=`) や中間バイトが付いた CSI は、終端バイトが
        // 同じでも別の命令。`CSI > 4 ; 2 m` (modifyOtherKeys) を SGR 4;2 と読むと
        // 控えの pen が下線+薄字になり、書き戻しで画面全体に下線が乗る。
        // `CSI > 1 u` / `CSI < u` (kitty 鍵盤プロトコル) も 'u' = カーソル復帰では
        // ない。扱うのは `?` の h/l (DECSET/DECRST) だけで、他は読み飛ばす。
        // DECSCUSR (`CSI Ps SP q`)。格子は変わらないが、モードの合図に使っている
        // カーソルの形を子アプリが奪ったことになるので印を付ける。
        if action == 'q' && intermediates.first() == Some(&b' ') {
            self.cursor_style_touched = true;
            return;
        }
        let private = intermediates.first() == Some(&b'?');
        let understood = intermediates.is_empty() || (private && matches!(action, 'h' | 'l'));
        if !understood {
            return;
        }
        match action {
            'A' => {
                self.row = self.row.saturating_sub(arg(params, 0, 1));
                self.wrap_pending = false;
            }
            'B' => {
                self.row = (self.row + arg(params, 0, 1)).min(self.rows - 1);
                self.wrap_pending = false;
            }
            'C' => {
                self.col = (self.col + arg(params, 0, 1)).min(self.cols - 1);
                self.wrap_pending = false;
            }
            'D' => {
                self.col = self.col.saturating_sub(arg(params, 0, 1));
                self.wrap_pending = false;
            }
            'E' => {
                self.row = (self.row + arg(params, 0, 1)).min(self.rows - 1);
                self.col = 0;
                self.wrap_pending = false;
            }
            'F' => {
                self.row = self.row.saturating_sub(arg(params, 0, 1));
                self.col = 0;
                self.wrap_pending = false;
            }
            'G' | '`' => {
                self.col = (arg(params, 0, 1) - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            'd' => {
                self.row = (arg(params, 0, 1) - 1).min(self.rows - 1);
                self.wrap_pending = false;
            }
            'H' | 'f' => {
                self.row = (arg(params, 0, 1) - 1).min(self.rows - 1);
                self.col = (arg(params, 1, 1) - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            'J' => {
                let mode = arg(params, 0, 0).min(3);
                let (row, cols, rows) = (self.row, self.cols, self.rows);
                match mode {
                    0 => {
                        self.clear_cells(row, self.col, cols);
                        for r in row + 1..rows {
                            self.clear_cells(r, 0, cols);
                        }
                    }
                    1 => {
                        for r in 0..row {
                            self.clear_cells(r, 0, cols);
                        }
                        self.clear_cells(row, 0, self.col + 1);
                    }
                    _ => {
                        for r in 0..rows {
                            self.clear_cells(r, 0, cols);
                        }
                    }
                }
            }
            'K' => {
                let mode = arg(params, 0, 0);
                let (row, col, cols) = (self.row, self.col, self.cols);
                match mode {
                    0 => self.clear_cells(row, col, cols),
                    1 => self.clear_cells(row, 0, col + 1),
                    _ => self.clear_cells(row, 0, cols),
                }
            }
            'L' => {
                let n = arg(params, 0, 1);
                if self.row >= self.scroll_top && self.row <= self.scroll_bot {
                    let saved = self.scroll_top;
                    self.scroll_top = self.row;
                    self.scroll_down(n);
                    self.scroll_top = saved;
                }
            }
            'M' => {
                let n = arg(params, 0, 1);
                if self.row >= self.scroll_top && self.row <= self.scroll_bot {
                    let saved = self.scroll_top;
                    self.scroll_top = self.row;
                    self.scroll_up(n);
                    self.scroll_top = saved;
                }
            }
            '@' => {
                let n = arg(params, 0, 1);
                let (row, col, cols) = (self.row, self.col, self.cols);
                if let Some(r) = self.grid.get_mut(row) {
                    for _ in 0..n {
                        if col < r.len() {
                            r.insert(col, Cell::default());
                            r.truncate(cols);
                        }
                    }
                }
            }
            'P' => {
                let n = arg(params, 0, 1);
                let (row, col, cols) = (self.row, self.col, self.cols);
                if let Some(r) = self.grid.get_mut(row) {
                    for _ in 0..n {
                        if col < r.len() {
                            r.remove(col);
                            r.push(Cell::default());
                        }
                    }
                    r.truncate(cols);
                }
            }
            'X' => {
                let n = arg(params, 0, 1);
                let (row, col) = (self.row, self.col);
                self.clear_cells(row, col, col + n);
            }
            'S' => {
                let n = arg(params, 0, 1);
                self.scroll_up(n);
            }
            'T' => {
                let n = arg(params, 0, 1);
                self.scroll_down(n);
            }
            'm' => self.set_sgr(params),
            'r' => {
                let top = arg(params, 0, 1) - 1;
                let bot = arg(params, 1, self.rows as u16) - 1;
                if top < bot && bot < self.rows {
                    self.scroll_top = top;
                    self.scroll_bot = bot;
                }
                self.row = 0;
                self.col = 0;
            }
            's' => self.saved_cursor = (self.row, self.col, self.pen),
            'u' => {
                let (r, c, a) = self.saved_cursor;
                self.row = r.min(self.rows - 1);
                self.col = c.min(self.cols - 1);
                self.pen = a;
            }
            'h' | 'l' => {
                let set = action == 'h';
                if !private {
                    return;
                }
                for p in params.iter() {
                    match p.first().copied().unwrap_or(0) {
                        7 => self.autowrap = set,
                        25 => self.cursor_visible = set,
                        47 | 1047 | 1049 => self.set_alt_screen(set),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.saved_cursor = (self.row, self.col, self.pen),
            b'8' => {
                let (r, c, a) = self.saved_cursor;
                self.row = r.min(self.rows - 1);
                self.col = c.min(self.cols - 1);
                self.pen = a;
            }
            b'D' => self.linefeed(),
            b'E' => {
                self.col = 0;
                self.linefeed();
            }
            b'M' => {
                if self.row == self.scroll_top {
                    self.scroll_down(1);
                } else {
                    self.row = self.row.saturating_sub(1);
                }
                self.wrap_pending = false;
            }
            b'c' => {
                let (rows, cols) = (self.rows, self.cols);
                *self = Screen::new(rows, cols);
            }
            _ => {}
        }
    }
}

impl Screen {
    fn set_alt_screen(&mut self, on: bool) {
        if on == self.alt_screen {
            return;
        }
        if on {
            self.saved_grid = Some(std::mem::replace(
                &mut self.grid,
                new_grid(self.rows, self.cols),
            ));
            self.alt_screen = true;
        } else {
            if let Some(g) = self.saved_grid.take() {
                self.grid = g;
            }
            self.alt_screen = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vte::Parser;

    fn feed(s: &mut Screen, bytes: &str) {
        let mut p = Parser::new();
        p.advance(s, bytes.as_bytes());
    }

    #[test]
    fn private_csi_is_not_mistaken_for_sgr_or_cursor_restore() {
        let mut s = Screen::new(5, 20);
        feed(&mut s, "\x1b[s"); // 控えを (0,0) に取る
        feed(&mut s, "abc");
        // modifyOtherKeys。SGR 4;2 (下線+薄字) と読んではいけない。
        feed(&mut s, "\x1b[>4;2m");
        assert_eq!(s.pen, Attr::default());
        // kitty 鍵盤プロトコル。'u' = カーソル復帰と読んではいけない。
        feed(&mut s, "\x1b[>1u");
        feed(&mut s, "\x1b[<u");
        assert_eq!((s.row, s.col), (0, 3));
        // 素の SGR とカーソル復帰はそのまま効く
        feed(&mut s, "\x1b[4m");
        assert_eq!(s.pen.flags & UNDERLINE, UNDERLINE);
        feed(&mut s, "\x1b[u");
        assert_eq!((s.row, s.col), (0, 0));
    }

    #[test]
    fn notices_when_the_child_takes_over_the_cursor_look() {
        let mut s = Screen::new(5, 20);
        feed(&mut s, "abc\x1b[?25l\x1b]0;title\x07");
        assert!(!s.take_cursor_style_touched());
        // DECSCUSR (形)
        feed(&mut s, "\x1b[5 q");
        assert!(s.take_cursor_style_touched());
        assert!(!s.take_cursor_style_touched(), "印は一度で落ちる");
        // OSC 12 (色) と OSC 112 (既定へ戻す)
        feed(&mut s, "\x1b]12;#ff0000\x07");
        assert!(s.take_cursor_style_touched());
        feed(&mut s, "\x1b]112\x07");
        assert!(s.take_cursor_style_touched());
        // 格子とカーソル位置には影響しない
        assert_eq!((s.row, s.col), (0, 3));
    }

    #[test]
    fn intermediate_bytes_are_skipped() {
        let mut s = Screen::new(5, 20);
        feed(&mut s, "xy");
        // DECSCUSR。'q' は元々扱わないが、中間バイト付きが素通りしないことを見る。
        feed(&mut s, "\x1b[2 q");
        // DECSTR。'p' も同様。
        feed(&mut s, "\x1b[!p");
        assert_eq!((s.row, s.col), (0, 2));
        assert_eq!(s.pen, Attr::default());
        // DECSET/DECRST は引き続き効く
        feed(&mut s, "\x1b[?25l");
        assert!(!s.cursor_visible);
    }

    #[test]
    fn tracks_cursor_and_text() {
        let mut s = Screen::new(5, 20);
        feed(&mut s, "hello");
        assert_eq!((s.row, s.col), (0, 5));
        assert_eq!(s.cell(0, 0).ch, 'h');
    }

    #[test]
    fn handles_wide_chars() {
        let mut s = Screen::new(5, 20);
        feed(&mut s, "あい");
        assert_eq!(s.col, 4);
        assert_eq!(s.cell(0, 0).ch, 'あ');
        assert_eq!(s.cell(0, 1).width, 0);
        assert_eq!(s.cell(0, 2).ch, 'い');
    }

    #[test]
    fn restores_region_with_attributes() {
        let mut s = Screen::new(5, 20);
        feed(&mut s, "\x1b[31mab\x1b[0mcd");
        let out = s.restore_region(0, 0, 4);
        assert!(out.starts_with("\x1b[1;1H"));
        assert!(out.contains(";31m"));
        assert!(out.ends_with("cd"));
    }

    /// 子の出力が文字の途中で切れても、控えの画面に置換文字が入らない。
    ///
    /// 子の出力は 8192 バイトずつ読む。かなは 3 バイトなので境界は文字の途中に
    /// 落ちる。パーサが持ち越さないと控えに U+FFFD が入り、重ね描きを消すときに
    /// **画面へ書き戻されてしまう**。
    #[test]
    fn split_utf8_output_keeps_the_grid_intact() {
        let text = "ただ一点、私の計算違いでしたら申し訳ないのですが、";
        let bytes = text.as_bytes();
        for at in 1..bytes.len() {
            let mut s = Screen::new(5, 80);
            let mut p = Parser::new();
            p.advance(&mut s, &bytes[..at]);
            p.advance(&mut s, &bytes[at..]);
            let shown = s.restore_region(0, 0, text.chars().count() * 2);
            assert!(
                !shown.contains('\u{fffd}'),
                "境界 {at} で置換文字が入った: {shown:?}"
            );
        }
    }

    #[test]
    fn restore_expands_over_wide_char() {
        let mut s = Screen::new(5, 20);
        feed(&mut s, "あい");
        // 全角の後続セルから始めても文字が欠けない
        let out = s.restore_region(0, 1, 1);
        assert!(out.contains('あ'));
    }

    #[test]
    fn wraps_and_scrolls() {
        let mut s = Screen::new(2, 3);
        feed(&mut s, "abcdef");
        assert_eq!(s.cell(1, 0).ch, 'd');
        feed(&mut s, "ghi");
        assert_eq!(s.cell(0, 0).ch, 'd');
        assert_eq!(s.cell(1, 0).ch, 'g');
    }

    #[test]
    fn erase_line_clears_cells() {
        let mut s = Screen::new(3, 10);
        feed(&mut s, "abcdef\r\x1b[K");
        assert_eq!(s.cell(0, 0).ch, ' ');
    }

    #[test]
    fn alt_screen_restores_main() {
        let mut s = Screen::new(3, 10);
        feed(&mut s, "main");
        feed(&mut s, "\x1b[?1049h");
        feed(&mut s, "\x1b[Halt");
        assert_eq!(s.cell(0, 0).ch, 'a');
        feed(&mut s, "\x1b[?1049l");
        assert_eq!(s.cell(0, 0).ch, 'm');
    }
}
