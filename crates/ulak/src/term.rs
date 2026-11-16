//! Minimal terminal screen over `vte`. Enough for the hub's full-redraw `PaneOutput`
//! (`ESC[H ESC[2J` + `capture-pane -e` text): print, CR/LF/BS/TAB, cursor moves, erase, SGR.
use vte::{Params, Parser, Perform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// 0-15 are the ANSI colors, 16-255 the xterm palette.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

const BLANK: Cell = Cell {
    ch: ' ',
    style: Style {
        fg: None,
        bg: None,
        bold: false,
    },
};

/// A run of equal-styled text on one row, for the view to draw as one text node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: Style,
    /// The cursor block sits on this run (a single cell); draw it inverted.
    pub cursor: bool,
}

struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    cx: usize,
    cy: usize,
    pen: Style,
}

pub struct Screen {
    grid: Grid,
    parser: Parser,
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Screen({}x{})", self.grid.cols, self.grid.rows)
    }
}

impl Default for Screen {
    fn default() -> Self {
        Self::new(120, 40)
    }
}

/// How the daemon starts every full-screen frame.
const REDRAW: &[u8] = b"\x1b[H\x1b[2J";

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Self {
            grid: Grid {
                cols,
                rows,
                cells: vec![BLANK; cols * rows],
                cx: 0,
                cy: 0,
                pen: Style::default(),
            },
            parser: Parser::new(),
        }
    }

    /// A full redraw (`ESC[H ESC[2J` + the whole pane, as the daemon sends it) first resizes the
    /// screen to the pane it shows: rows = lines, cols = longest line (escapes included, so a
    /// little too wide at worst). The pane size is fixed daemon-side; this just follows it.
    pub fn feed(&mut self, bytes: &[u8]) {
        if let Some(body) = bytes.strip_prefix(REDRAW) {
            let mut lines: Vec<&[u8]> = body.split(|&b| b == b'\n').collect();
            if lines.last().is_some_and(|l| l.is_empty()) {
                lines.pop();
            }
            let cols = lines
                .iter()
                .map(|l| l.len())
                .max()
                .unwrap_or(0)
                .clamp(80, 400);
            // One spare row: the final CRLF of a full pane must not scroll it.
            let rows = (lines.len() + 1).min(200);
            if (cols, rows) != (self.grid.cols, self.grid.rows) {
                *self = Self::new(cols, rows);
            }
        }
        self.parser.advance(&mut self.grid, bytes);
    }

    pub fn size(&self) -> (usize, usize) {
        (self.grid.cols, self.grid.rows)
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.grid.cx, self.grid.cy)
    }

    pub fn cell(&self, col: usize, row: usize) -> Option<Cell> {
        (col < self.grid.cols && row < self.grid.rows)
            .then(|| self.grid.cells[row * self.grid.cols + col])
    }

    fn row_cells(&self, row: usize) -> &[Cell] {
        &self.grid.cells[row * self.grid.cols..(row + 1) * self.grid.cols]
    }

    /// Row text without trailing blanks.
    pub fn line(&self, row: usize) -> String {
        if row >= self.grid.rows {
            return String::new();
        }
        let s: String = self.row_cells(row).iter().map(|c| c.ch).collect();
        s.trim_end().to_owned()
    }

    pub fn lines(&self) -> Vec<String> {
        (0..self.grid.rows).map(|r| self.line(r)).collect()
    }

    /// Row split into style runs; trailing unstyled blanks are dropped.
    pub fn runs(&self, row: usize) -> Vec<Run> {
        self.runs_with_cursor(row, None)
    }

    /// Like `runs`, but the cell at `cursor` (column) becomes its own run with `cursor` set,
    /// even past the end of the text.
    pub fn runs_with_cursor(&self, row: usize, cursor: Option<usize>) -> Vec<Run> {
        if row >= self.grid.rows {
            return Vec::new();
        }
        let cells = self.row_cells(row);
        let mut end = cells.iter().rposition(|c| *c != BLANK).map_or(0, |i| i + 1);
        if let Some(c) = cursor.filter(|&c| c < cells.len()) {
            end = end.max(c + 1);
        }
        let mut out: Vec<Run> = Vec::new();
        for (i, c) in cells[..end].iter().enumerate() {
            let cur = cursor == Some(i);
            match out.last_mut() {
                Some(r) if r.style == c.style && !r.cursor && !cur => r.text.push(c.ch),
                _ => out.push(Run {
                    text: c.ch.to_string(),
                    style: c.style,
                    cursor: cur,
                }),
            }
        }
        out
    }
}

impl Grid {
    fn idx(&self, col: usize, row: usize) -> usize {
        row * self.cols + col
    }

    fn scroll_up(&mut self) {
        self.cells.drain(..self.cols);
        self.cells.extend(std::iter::repeat_n(BLANK, self.cols));
    }

    fn line_feed(&mut self) {
        if self.cy + 1 == self.rows {
            self.scroll_up();
        } else {
            self.cy += 1;
        }
    }

    fn fill(&mut self, from: usize, to: usize) {
        // Erased cells keep the pen's background, like xterm's bce.
        let blank = Cell {
            ch: ' ',
            style: Style {
                bg: self.pen.bg,
                ..Style::default()
            },
        };
        self.cells[from..to].fill(blank);
    }

    fn sgr(&mut self, params: &Params) {
        let flat: Vec<u16> = params.iter().flat_map(|p| p.iter().copied()).collect();
        if flat.is_empty() {
            self.pen = Style::default();
        }
        let mut i = 0;
        while i < flat.len() {
            let p = flat[i];
            i += 1;
            match p {
                0 => self.pen = Style::default(),
                1 => self.pen.bold = true,
                22 => self.pen.bold = false,
                30..=37 => self.pen.fg = Some(Color::Indexed((p - 30) as u8)),
                40..=47 => self.pen.bg = Some(Color::Indexed((p - 40) as u8)),
                90..=97 => self.pen.fg = Some(Color::Indexed((p - 90 + 8) as u8)),
                100..=107 => self.pen.bg = Some(Color::Indexed((p - 100 + 8) as u8)),
                39 => self.pen.fg = None,
                49 => self.pen.bg = None,
                38 | 48 => {
                    let color = match flat.get(i) {
                        Some(5) => flat.get(i + 1).map(|&n| (Color::Indexed(n as u8), 2)),
                        Some(2) => match flat.get(i + 1..i + 4) {
                            Some(&[r, g, b]) => Some((Color::Rgb(r as u8, g as u8, b as u8), 4)),
                            _ => None,
                        },
                        _ => None,
                    };
                    let Some((c, used)) = color else { break };
                    i += used;
                    if p == 38 {
                        self.pen.fg = Some(c);
                    } else {
                        self.pen.bg = Some(c);
                    }
                }
                _ => {}
            }
        }
    }
}

impl Perform for Grid {
    fn print(&mut self, ch: char) {
        if self.cx == self.cols {
            self.cx = 0;
            self.line_feed();
        }
        let i = self.idx(self.cx, self.cy);
        self.cells[i] = Cell {
            ch,
            style: self.pen,
        };
        self.cx += 1;
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => self.line_feed(),
            b'\r' => self.cx = 0,
            0x08 => self.cx = self.cx.saturating_sub(1),
            b'\t' => self.cx = ((self.cx / 8 + 1) * 8).min(self.cols - 1),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], _ignore: bool, action: char) {
        if !inter.is_empty() {
            return;
        }
        let arg = |n: usize, default: usize| {
            params
                .iter()
                .nth(n)
                .and_then(|p| p.first().copied())
                .filter(|&v| v != 0)
                .map_or(default, usize::from)
        };
        let (cols, rows) = (self.cols, self.rows);
        match action {
            'm' => self.sgr(params),
            'H' | 'f' => {
                self.cy = (arg(0, 1) - 1).min(rows - 1);
                self.cx = (arg(1, 1) - 1).min(cols - 1);
            }
            'A' => self.cy = self.cy.saturating_sub(arg(0, 1)),
            'B' => self.cy = (self.cy + arg(0, 1)).min(rows - 1),
            'C' => self.cx = (self.cx + arg(0, 1)).min(cols - 1),
            'D' => self.cx = self.cx.saturating_sub(arg(0, 1)),
            'G' => self.cx = (arg(0, 1) - 1).min(cols - 1),
            'J' => {
                let here = self.idx(self.cx.min(cols - 1), self.cy);
                // mode 0 (default) = cursor to end, 1 = start to cursor, 2/3 = all
                let mode = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                match mode {
                    0 => self.fill(here, cols * rows),
                    1 => self.fill(0, here + 1),
                    _ => self.fill(0, cols * rows),
                }
            }
            'K' => {
                let start = self.cy * cols;
                let here = start + self.cx.min(cols - 1);
                let mode = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                match mode {
                    0 => self.fill(here, start + cols),
                    1 => self.fill(start, here + 1),
                    _ => self.fill(start, start + cols),
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(bytes: &str) -> Screen {
        let mut s = Screen::new(10, 4);
        s.feed(bytes.as_bytes());
        s
    }

    #[test]
    fn prints_and_moves_cursor() {
        let s = screen("hi");
        assert_eq!(s.line(0), "hi");
        assert_eq!(s.cursor(), (2, 0));
    }

    #[test]
    fn crlf_starts_next_row_but_lf_alone_keeps_column() {
        let s = screen("ab\r\ncd");
        assert_eq!(s.lines()[..2], ["ab", "cd"]);
        let s = screen("ab\ncd");
        assert_eq!(s.line(1), "  cd");
    }

    #[test]
    fn wraps_at_the_right_edge_and_scrolls_at_the_bottom() {
        let s = screen("0123456789X");
        assert_eq!((s.line(0), s.line(1)), ("0123456789".into(), "X".into()));
        let s = screen("1\r\n2\r\n3\r\n4\r\n5");
        assert_eq!(s.lines(), ["2", "3", "4", "5"]);
    }

    #[test]
    fn cursor_home_and_erase_display_make_a_full_redraw() {
        let mut s = screen("old\r\nstuff");
        s.feed(b"\x1b[H\x1b[2Jnew\r\nscreen");
        assert_eq!(s.lines(), ["new", "screen", ""]);
    }

    #[test]
    fn cursor_position_is_one_based() {
        let s = screen("\x1b[2;3Hx");
        assert_eq!(s.cell(2, 1).map(|c| c.ch), Some('x'));
        let s = screen("abc\x1b[Hz");
        assert_eq!(s.line(0), "zbc");
    }

    #[test]
    fn erase_line_modes() {
        let mut s = screen("abcdef\x1b[3G\x1b[K");
        assert_eq!(s.line(0), "ab");
        s = screen("abcdef\x1b[3G\x1b[1K");
        assert_eq!(s.line(0), "   def");
        s = screen("abcdef\x1b[2K");
        assert_eq!(s.line(0), "");
    }

    #[test]
    fn erase_display_from_cursor() {
        let s = screen("ab\r\ncd\x1b[2;1H\x1b[J");
        assert_eq!(s.lines()[..2], ["ab", ""]);
    }

    #[test]
    fn sgr_colors_and_bold_are_stored_per_cell() {
        let s = screen("\x1b[1;31mR\x1b[0mn\x1b[38;5;200mI\x1b[48;2;1;2;3mT\x1b[39;49m.");
        let st = |c| s.cell(c, 0).unwrap().style;
        assert_eq!(
            st(0),
            Style {
                fg: Some(Color::Indexed(1)),
                bg: None,
                bold: true
            }
        );
        assert_eq!(st(1), Style::default());
        assert_eq!(st(2).fg, Some(Color::Indexed(200)));
        assert_eq!(st(3).bg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(st(4), Style::default());
    }

    #[test]
    fn bright_colors_map_to_8_to_15() {
        let s = screen("\x1b[92;101mx");
        let st = s.cell(0, 0).unwrap().style;
        assert_eq!(
            (st.fg, st.bg),
            (Some(Color::Indexed(10)), Some(Color::Indexed(9)))
        );
    }

    #[test]
    fn runs_group_equal_styles_and_drop_trailing_blanks() {
        let s = screen("a\x1b[31mbc\x1b[0md   ");
        let r = s.runs(0);
        assert_eq!(
            r.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["a", "bc", "d"]
        );
        assert_eq!(r[1].style.fg, Some(Color::Indexed(1)));
    }

    #[test]
    fn utf8_split_across_feeds_survives() {
        let mut s = Screen::new(10, 2);
        let b = "é".as_bytes();
        s.feed(&b[..1]);
        s.feed(&b[1..]);
        assert_eq!(s.line(0), "é");
    }

    #[test]
    fn out_of_range_reads_are_empty() {
        let s = screen("x");
        assert_eq!(
            (s.cell(99, 0), s.line(99), s.runs(99)),
            (None, String::new(), vec![])
        );
    }

    #[test]
    fn full_redraw_resizes_to_the_pane() {
        let mut s = Screen::default();
        let mut frame = b"\x1b[H\x1b[2JTOP\r\n".to_vec();
        frame.extend(std::iter::repeat_n(b"\r\n", 48).flatten());
        frame.extend(b"BOTTOM\r\n");
        s.feed(&frame);
        assert_eq!(s.size().1, 51);
        assert_eq!(s.line(0), "TOP");
        assert_eq!(s.line(49), "BOTTOM");
    }

    #[test]
    fn cursor_cell_is_its_own_run_even_past_the_text() {
        let mut s = Screen::new(10, 2);
        s.feed(b"ab");
        let runs = s.runs_with_cursor(0, Some(2));
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].text.as_str(), runs[0].cursor), ("ab", false));
        assert_eq!((runs[1].text.as_str(), runs[1].cursor), (" ", true));
        let mid = s.runs_with_cursor(0, Some(0));
        assert_eq!(mid[0].text, "a");
        assert!(mid[0].cursor);
        assert_eq!(mid[1].text, "b");
    }
}
