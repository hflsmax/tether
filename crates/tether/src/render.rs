use std::io::Write;

use crossterm::{cursor, queue, style, terminal};
use tether_protocol::{CellFlags, Color, ColorKind, CursorShape, Row, SessionState};

/// Render a structured terminal snapshot to the local terminal.
pub fn render_snapshot(state: &SessionState, out: &mut impl Write) -> std::io::Result<()> {
    // Clear screen
    queue!(out, terminal::Clear(terminal::ClearType::All))?;

    // Render visible rows
    for (row_idx, row) in state.visible_rows.iter().enumerate() {
        queue!(out, cursor::MoveTo(0, row_idx as u16))?;
        render_row(row, out)?;
    }

    // Position cursor
    if state.cursor.visible {
        queue!(
            out,
            cursor::MoveTo(state.cursor.col, state.cursor.row),
            cursor::Show
        )?;
        match state.cursor.shape {
            CursorShape::Block => queue!(out, cursor::SetCursorStyle::DefaultUserShape)?,
            CursorShape::Underline => queue!(out, cursor::SetCursorStyle::SteadyUnderScore)?,
            CursorShape::Bar => queue!(out, cursor::SetCursorStyle::SteadyBar)?,
        }
    } else {
        queue!(out, cursor::Hide)?;
    }

    out.flush()?;
    Ok(())
}

fn render_row(row: &Row, out: &mut impl Write) -> std::io::Result<()> {
    // Track current style to minimize escape sequences
    let mut current_fg: Option<Color> = None;
    let mut current_bg: Option<Color> = None;
    let mut current_flags = CellFlags::empty();

    for cell in &row.cells {
        // Apply style changes
        let need_reset = cell.flags != current_flags;
        if need_reset {
            queue!(out, style::SetAttribute(style::Attribute::Reset))?;
            current_fg = None;
            current_bg = None;
            current_flags = CellFlags::empty();
        }

        if Some(cell.fg) != current_fg {
            queue!(out, style::SetForegroundColor(to_crossterm_color(&cell.fg)))?;
            current_fg = Some(cell.fg);
        }
        if Some(cell.bg) != current_bg {
            queue!(out, style::SetBackgroundColor(to_crossterm_color(&cell.bg)))?;
            current_bg = Some(cell.bg);
        }

        if cell.flags != current_flags {
            apply_flags(cell.flags, out)?;
            current_flags = cell.flags;
        }

        write!(out, "{}", cell.c)?;
    }

    // Reset at end of row
    queue!(out, style::SetAttribute(style::Attribute::Reset))?;
    Ok(())
}

fn to_crossterm_color(color: &Color) -> style::Color {
    match color.kind {
        ColorKind::Default => style::Color::Reset,
        ColorKind::Indexed(idx) => style::Color::AnsiValue(idx),
        ColorKind::Rgb => style::Color::Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        },
    }
}

fn apply_flags(flags: CellFlags, out: &mut impl Write) -> std::io::Result<()> {
    if flags.contains(CellFlags::BOLD) {
        queue!(out, style::SetAttribute(style::Attribute::Bold))?;
    }
    if flags.contains(CellFlags::DIM) {
        queue!(out, style::SetAttribute(style::Attribute::Dim))?;
    }
    if flags.contains(CellFlags::ITALIC) {
        queue!(out, style::SetAttribute(style::Attribute::Italic))?;
    }
    if flags.contains(CellFlags::UNDERLINE) {
        queue!(out, style::SetAttribute(style::Attribute::Underlined))?;
    }
    if flags.contains(CellFlags::INVERSE) {
        queue!(out, style::SetAttribute(style::Attribute::Reverse))?;
    }
    if flags.contains(CellFlags::STRIKETHROUGH) {
        queue!(
            out,
            style::SetAttribute(style::Attribute::CrossedOut)
        )?;
    }
    if flags.contains(CellFlags::HIDDEN) {
        queue!(out, style::SetAttribute(style::Attribute::Hidden))?;
    }
    Ok(())
}
