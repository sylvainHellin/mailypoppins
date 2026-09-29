//! Shared modal / panel primitives (#0032, ported from herdr's
//! `src/ui/widgets.rs`).
//!
//! Before this module every overlay re-implemented the same `Flex::Center`
//! horizontal+vertical layout dance plus its own `Clear` + bordered `Block`,
//! which drifted into subtly inconsistent margins and border styles. These
//! five primitives centralize that so overlays share one shell:
//!
//! - [`render_panel_shell`] \u2014 `Clear` + rounded bordered block, returns inner
//!   `Rect`;
//! - [`centered_modal_rect`] / [`render_modal_shell`] \u2014 centered, clamped
//!   popup;
//! - [`modal_stack_areas`] \u2014 split a modal's inner area into
//!   header / content / footer rows;
//! - [`render_action_button`] \u2014 the ` esc close ` accent chip;
//! - `panel_contrast_fg` lives on [`super::super::theme::Theme`].

use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Dim every cell of `area` by OR-ing in `Modifier::DIM`, so a modal drawn on
/// top of it visually floats above the (now recessed) main view. Call once,
/// before drawing the overlay. (#0032, herdr `dim_background`.)
pub(super) fn dim_background(frame: &mut Frame, area: Rect) {
    let buf = frame.buffer_mut();
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let cell = &mut buf[(x, y)];
            cell.set_style(cell.style().add_modifier(Modifier::DIM));
        }
    }
}

/// Clear `area`, draw a rounded bordered block filled with `bg`, and return
/// the inner content `Rect`. Returns `None` when the area is too small to
/// hold a border. The caller supplies `border_color`/`bg` from the theme so
/// each overlay keeps its distinguishing accent (warning/error/success/...).
pub(super) fn render_panel_shell(
    frame: &mut Frame,
    area: Rect,
    border_color: Color,
    bg: Color,
) -> Option<Rect> {
    if area.width < 2 || area.height < 2 {
        return None;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    Some(inner)
}

/// Centered popup rect of at most `popup_w` x `popup_h`, clamped to fit
/// `area` with a small margin. Mirrors the `Flex::Center` +
/// `.min(area.width - 4)` dance the overlays used inline.
pub(super) fn centered_modal_rect(area: Rect, popup_w: u16, popup_h: u16) -> Rect {
    let popup_w = popup_w.min(area.width.saturating_sub(4));
    let popup_h = popup_h.min(area.height.saturating_sub(2));

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(popup_w)])
        .flex(Flex::Center)
        .split(area);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(popup_h)])
        .flex(Flex::Center)
        .split(horizontal[0]);
    vertical[0]
}

/// Compute a centered popup, then draw its shell. Returns the popup rect and
/// its inner content rect (or `None` if it could not be drawn).
pub(super) fn render_modal_shell(
    frame: &mut Frame,
    area: Rect,
    popup_w: u16,
    popup_h: u16,
    border_color: Color,
    bg: Color,
) -> Option<(Rect, Rect)> {
    let popup = centered_modal_rect(area, popup_w, popup_h);
    let inner = render_panel_shell(frame, popup, border_color, bg)?;
    Some((popup, inner))
}

/// A modal's inner area split into stacked rows: a fixed-height header, a
/// flexible content region, and an optional fixed-height footer. Rows of
/// height 0 are omitted.
#[derive(Debug, Clone, Copy)]
pub(super) struct ModalStackAreas {
    pub header: Rect,
    pub content: Rect,
    pub footer: Option<Rect>,
}

/// Split `inner` vertically into header / content / footer. `footer_height`
/// of 0 yields no footer row.
pub(super) fn modal_stack_areas(
    inner: Rect,
    header_height: u16,
    footer_height: u16,
) -> ModalStackAreas {
    if footer_height == 0 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(header_height), Constraint::Min(0)])
            .split(inner);
        return ModalStackAreas {
            header: rows[0],
            content: rows[1],
            footer: None,
        };
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(0),
            Constraint::Length(footer_height),
        ])
        .split(inner);
    ModalStackAreas {
        header: rows[0],
        content: rows[1],
        footer: Some(rows[2]),
    }
}

/// Render a compact accent chip like ` esc close `, centered in `rect`.
/// `hint` is the key (e.g. `"esc"`); `label` the action word. The chip uses
/// `theme::active().panel_contrast_fg()` on an accent background so it stays
/// readable across themes (including `Color::Reset` terminals).
pub(super) fn render_action_button(
    frame: &mut Frame,
    rect: Rect,
    hint: Option<&str>,
    label: &str,
    accent: Color,
) {
    let text = match hint {
        Some(hint) => format!(" {hint} {label} "),
        None => format!(" {label} "),
    };
    let style = Style::default()
        .bg(accent)
        .fg(super::super::theme::active().panel_contrast_fg())
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(text).style(style).alignment(Alignment::Center),
        rect,
    );
}
