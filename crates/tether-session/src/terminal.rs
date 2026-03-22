use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::vte::ansi;
use tether_protocol::{
    Cell, CellFlags, Color, ColorKind, CursorShape, CursorState, Row, ScreenMode, SessionState,
};

/// Event listener that discards events (daemon doesn't need to react to title changes etc.)
struct NullListener;

impl EventListener for NullListener {
    fn send_event(&self, _event: Event) {}
}

/// Wraps alacritty_terminal::Term for state tracking and snapshot extraction.
pub struct TerminalModel {
    term: Term<NullListener>,
    cols: u16,
    rows: u16,
    viewport_offset: u32,
}

impl TerminalModel {
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Self {
        // Clamp to at least 2x2 — alacritty panics on zero-sized terminals
        let cols = cols.max(2);
        let rows = rows.max(2);
        let size = TermSize::new(cols as usize, rows as usize);
        let config = alacritty_terminal::term::Config {
            scrolling_history: scrollback_lines,
            ..Default::default()
        };
        let term = Term::new(config, &size, NullListener);
        Self {
            term,
            cols,
            rows,
            viewport_offset: 0,
        }
    }

    /// Feed raw PTY output bytes into the terminal emulator.
    pub fn process(&mut self, bytes: &[u8]) {
        let mut processor = ansi::Processor::<ansi::StdSyncHandler>::new();
        processor.advance(&mut self.term, bytes);
    }

    /// Resize the terminal model.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        let size = TermSize::new(cols as usize, rows as usize);
        self.term.resize(size);
    }

    /// Record the viewport offset (called when client detaches).
    pub fn set_viewport_offset(&mut self, offset: u32) {
        self.viewport_offset = offset;
    }

    /// Extract a structured snapshot of the current terminal state.
    pub fn snapshot(&self, max_scrollback_rows: usize) -> SessionState {
        let grid = self.term.grid();
        let screen_mode = if self.term.mode().contains(term::TermMode::ALT_SCREEN) {
            ScreenMode::Alternate
        } else {
            ScreenMode::Main
        };

        // Extract visible rows (Line(0) through Line(screen_lines - 1))
        let mut visible_rows = Vec::with_capacity(self.rows as usize);
        for i in 0..grid.screen_lines() {
            let line = &grid[Line(i as i32)];
            visible_rows.push(convert_row(line, self.cols));
        }

        // Extract scrollback (only for main screen)
        // History lines are Line(-1) through Line(-(history_size))
        let mut scrollback = Vec::new();
        if screen_mode == ScreenMode::Main {
            let history = grid.history_size();
            let take = max_scrollback_rows.min(history);
            for i in 1..=take {
                let line = &grid[Line(-(i as i32))];
                scrollback.push(convert_row(line, self.cols));
            }
        }

        let cursor = &self.term.grid().cursor;
        let cursor_state = CursorState {
            row: cursor.point.line.0 as u16,
            col: cursor.point.column.0 as u16,
            visible: self.term.mode().contains(term::TermMode::SHOW_CURSOR),
            shape: convert_cursor_shape(self.term.cursor_style().shape),
        };

        SessionState {
            cols: self.cols,
            rows: self.rows,
            screen_mode,
            visible_rows,
            cursor: cursor_state,
            scrollback,
            viewport_offset: self.viewport_offset,
        }
    }
}

fn convert_row(
    line: &alacritty_terminal::grid::Row<alacritty_terminal::term::cell::Cell>,
    cols: u16,
) -> Row {
    let mut cells = Vec::with_capacity(cols as usize);
    for col_idx in 0..cols as usize {
        let cell = &line[alacritty_terminal::index::Column(col_idx)];
        cells.push(convert_cell(cell));
    }
    Row { cells }
}

fn convert_cell(cell: &alacritty_terminal::term::cell::Cell) -> Cell {
    let c = cell.c;
    let fg = convert_color(&cell.fg);
    let bg = convert_color(&cell.bg);
    let mut flags = CellFlags::empty();
    let cell_flags = cell.flags;
    if cell_flags.contains(alacritty_terminal::term::cell::Flags::BOLD) {
        flags |= CellFlags::BOLD;
    }
    if cell_flags.contains(alacritty_terminal::term::cell::Flags::ITALIC) {
        flags |= CellFlags::ITALIC;
    }
    if cell_flags.intersects(alacritty_terminal::term::cell::Flags::ALL_UNDERLINES) {
        flags |= CellFlags::UNDERLINE;
    }
    if cell_flags.contains(alacritty_terminal::term::cell::Flags::INVERSE) {
        flags |= CellFlags::INVERSE;
    }
    if cell_flags.contains(alacritty_terminal::term::cell::Flags::STRIKEOUT) {
        flags |= CellFlags::STRIKETHROUGH;
    }
    if cell_flags.contains(alacritty_terminal::term::cell::Flags::DIM) {
        flags |= CellFlags::DIM;
    }
    if cell_flags.contains(alacritty_terminal::term::cell::Flags::HIDDEN) {
        flags |= CellFlags::HIDDEN;
    }
    Cell { c, fg, bg, flags }
}

fn convert_color(color: &alacritty_terminal::vte::ansi::Color) -> Color {
    use alacritty_terminal::vte::ansi::NamedColor;
    match color {
        alacritty_terminal::vte::ansi::Color::Named(named) => {
            let idx = *named as u16;
            if idx >= NamedColor::Foreground as u16 {
                // Foreground, Background, Cursor, Dim* — these are semantic
                // colors that map to the terminal's defaults, not ANSI indices.
                Color::default()
            } else {
                Color { r: 0, g: 0, b: 0, kind: ColorKind::Indexed(idx as u8) }
            }
        }
        alacritty_terminal::vte::ansi::Color::Spec(rgb) => Color {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
            kind: ColorKind::Rgb,
        },
        alacritty_terminal::vte::ansi::Color::Indexed(idx) => Color {
            r: 0,
            g: 0,
            b: 0,
            kind: ColorKind::Indexed(*idx),
        },
    }
}

fn convert_cursor_shape(shape: alacritty_terminal::vte::ansi::CursorShape) -> CursorShape {
    match shape {
        alacritty_terminal::vte::ansi::CursorShape::Block => CursorShape::Block,
        alacritty_terminal::vte::ansi::CursorShape::Underline => CursorShape::Underline,
        alacritty_terminal::vte::ansi::CursorShape::Beam => CursorShape::Bar,
        _ => CursorShape::Block,
    }
}
