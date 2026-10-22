mod activity;
mod calendar;
mod compose;
mod contacts;
/// Pre-rewrite golden-frame capture (#0049 unit 0a); tests only.
///
/// `pub(crate)`, because the daemon-backed half of the suite lives in the root
/// crate's own tests since #0126 (P5-U10e) and builds its frames from this
/// module's fixtures: one fixture, two families, no drift.
#[cfg(any(test, feature = "test-support"))]
pub mod golden_frames;
mod headers;
mod list;
mod overlays;
pub mod preview;
mod search;
mod sidebar;
mod status;
mod util;
mod views;
mod widgets;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui::Frame;

use super::app::{App, Overlay, View};
use super::theme::{self, Theme};

/// Whether `theme` gets the outer frame around the whole TUI.
///
/// A theme that paints its own background would otherwise run straight into
/// the terminal's, with no edge between the two designs; the frame is that
/// edge. The `terminal` theme paints none (`bg` is `Color::Reset`), so there
/// is no seam to mark and the frame would cost two columns and two rows for
/// nothing.
pub(crate) fn has_outer_frame(theme: &Theme) -> bool {
    theme.bg != Color::Reset
}

/// Draw the outer frame for `theme` over `area` and return the area left
/// inside it, where everything else lays out: panes, hint bar, overlays.
///
/// The frame is a rounded border like every pane's, in the theme's unfocused
/// border colour, on the theme's ground; it carries no title. A theme without
/// a frame ([`has_outer_frame`]) gets `area` back untouched and nothing drawn.
/// The decision is read every frame, so the frame follows the active theme
/// without a flag of its own.
fn render_outer_frame(frame: &mut Frame, area: Rect, theme: &Theme) -> Rect {
    if !has_outer_frame(theme) {
        return area;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// The stacked regions of the herdr-style left column.
struct LeftColumn {
    sidebar: ratatui::layout::Rect,
    /// Email list (Mail view) or content host (non-Mail, narrow tier).
    middle: ratatui::layout::Rect,
    /// Activity-log panel, present only when toggled on in the Mail view.
    activity: Option<ratatui::layout::Rect>,
    /// Bottom-left view switcher (#0033), always present.
    switcher: ratatui::layout::Rect,
}

/// Split the left column into sidebar / middle / optional activity-log /
/// bottom-left view switcher. The activity-log slot is preserved from the
/// pre-#0033 layout (toggled by `!`) and only shown in the Mail view; the
/// switcher is pinned to the very bottom in every view.
fn split_left_column(app: &App, area: ratatui::layout::Rect, sidebar_height: u16) -> LeftColumn {
    let show_activity = app.show_activity_log && app.view == View::Mail;
    let mut constraints = vec![Constraint::Length(sidebar_height), Constraint::Min(0)];
    if show_activity {
        constraints.push(Constraint::Length(6));
    }
    constraints.push(Constraint::Length(views::SWITCHER_HEIGHT));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    if show_activity {
        LeftColumn {
            sidebar: rows[0],
            middle: rows[1],
            activity: Some(rows[2]),
            switcher: rows[3],
        }
    } else {
        LeftColumn {
            sidebar: rows[0],
            middle: rows[1],
            activity: None,
            switcher: rows[2],
        }
    }
}

/// Render the entire UI from the current app state.
pub fn view(app: &mut App, frame: &mut Frame) {
    // The outer frame (painted themes only) is drawn first, and from here on
    // `area` is its inside: no pane, popup or overlay dim ever touches it.
    // The width tiers below stay keyed on `app.terminal_width`, the
    // terminal's own size, so a classic 80-column terminal keeps the full
    // split with 78 columns inside the frame, and the key handler's own
    // narrow-tier check (`terminal_width < 80`) agrees with the layout.
    let area = render_outer_frame(frame, frame.area(), theme::active());

    // The preview body is not carried by the list entry; it is read from the
    // blob store for the selected message only (#0038 scope item 5). This is
    // the one place that holds `&mut App` immediately before a frame, so it is
    // where the memo is brought up to date. An unchanged selection costs a
    // key comparison. The invite behind the event card is memoised beside it
    // (#0038 scope item 6) and costs nothing for a message that is not one.
    app.refresh_preview_body();
    app.refresh_preview_invite();

    // Bottom row: the herdr-style mode/hint bar (#0032), drawn as a bordered
    // pane like every other pane. The former status bar is gone: its outbox
    // and sync badges were sticky by construction (a `failed` outbox row only
    // leaves on an explicit retry/discard) and the activity log already
    // carries the same events as they happen.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(status::HINT_BAR_HEIGHT),
        ])
        .split(area);

    let main_area = outer[0];
    let hint_area = outer[1];

    // Pane zoom (#TKT-0044): one pane owns the whole content area, the view
    // switcher and the other panes step aside, and the hint bar stays put --
    // a zoom must not cost the user the row that says how to leave it. Every
    // width tier collapses to the same single pane, so the narrow tier gets
    // zoom for free.
    if let Some(pane) = app.zoomed_pane() {
        render_zoomed_pane(app, frame, main_area, pane);
        status::render_hint_bar(app, frame, hint_area);
        render_overlays(app, frame, area);
        return;
    }

    let show_right = app.terminal_width >= 80;
    let show_sidebar = app.terminal_width >= 40;

    if show_right {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(main_area);

        let left_col = columns[0];
        let right_col = columns[1];

        if app.view == View::Mail {
            // Two border rows, one per mailbox, one spare, plus whatever the
            // #0071 sync-failure block needs when there is a failure to show.
            // The headers pane on the right is sized from the same number,
            // which is what keeps the two panes aligned.
            let sidebar_height = (app.mailboxes.len() as u16) + 3 + sidebar::sync_health_rows(app);
            let left = split_left_column(app, left_col, sidebar_height);

            sidebar::render_sidebar(app, frame, left.sidebar);
            list::render_email_list(app, frame, left.middle);
            if let Some(activity) = left.activity {
                sidebar::render_activity_log(app, frame, activity);
            }
            views::render_view_switcher(app, frame, left.switcher);

            let right_panels = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(sidebar_height), Constraint::Min(0)])
                .split(right_col);
            headers::render_headers(app, frame, right_panels[0]);
            preview::render_body(app, frame, right_panels[1]);
        } else {
            // Off Mail the mailbox sidebar carries nothing the view can act on,
            // and it used to sit above a blank left-middle slot (#TKT-0048).
            // Give the whole left column to the view's list and the right
            // column to its detail pane, mirroring the Mail list + preview
            // split, with only the view switcher pinned below.
            let left_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(views::SWITCHER_HEIGHT),
                ])
                .split(left_col);
            if app.view == View::Contacts {
                contacts::render_contacts_split(app, frame, left_rows[0], right_col);
            } else {
                calendar::render_calendar_split(app, frame, left_rows[0], right_col);
            }
            views::render_view_switcher(app, frame, left_rows[1]);
        }
    } else if show_sidebar && app.view == View::Mail {
        let sidebar_height = (app.mailboxes.len() as u16) + 2 + sidebar::sync_health_rows(app);
        let left = split_left_column(app, main_area, sidebar_height);

        sidebar::render_sidebar(app, frame, left.sidebar);
        list::render_email_list(app, frame, left.middle);
        if let Some(activity) = left.activity {
            sidebar::render_activity_log(app, frame, activity);
        }
        views::render_view_switcher(app, frame, left.switcher);
    } else if app.view == View::Mail {
        list::render_email_list(app, frame, main_area);
    } else {
        // Non-Mail without a right column (medium and narrow tiers): the mailbox
        // sidebar is Mail-only chrome, so the active view's content fills the
        // frame with the switcher pinned below (#TKT-0048). `render_contacts` /
        // `render_calendar` split themselves into list + detail when the single
        // area is wide enough, and show the list alone otherwise.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(views::SWITCHER_HEIGHT),
            ])
            .split(main_area);
        if app.view == View::Contacts {
            contacts::render_contacts(app, frame, rows[0]);
        } else {
            calendar::render_calendar(app, frame, rows[0]);
        }
        views::render_view_switcher(app, frame, rows[1]);
    }

    status::render_hint_bar(app, frame, hint_area);

    render_overlays(app, frame, area);
}

/// Draw one pane over the whole content area (#TKT-0044).
///
/// The panes keep their own renderers and their own borders, so a zoomed pane
/// is the pane the user already knows, wider: nothing about the content is
/// special-cased for the zoom, which is what keeps the two layouts from
/// drifting apart.
fn render_zoomed_pane(
    app: &mut App,
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    pane: super::app::Focus,
) {
    use super::app::Focus;
    match pane {
        Focus::Sidebar => sidebar::render_sidebar(app, frame, area),
        Focus::Headers => headers::render_headers(app, frame, area),
        Focus::Preview => preview::render_body(app, frame, area),
        // `Focus::Search` is folded into `List` by `App::zoom_target`, and
        // `ComposeWizard` never zooms; both are unreachable here.
        Focus::List | Focus::Search | Focus::ComposeWizard => {
            list::render_email_list(app, frame, area)
        }
    }
}

/// The single-overlay dispatch, shared by the split and zoomed layouts.
fn render_overlays(app: &mut App, frame: &mut Frame, area: ratatui::layout::Rect) {
    // Exactly one overlay at a time by construction (#0032): a single match
    // on `app.overlay` replaces the former if-cascade of independent overlay
    // flags. Dim the whole frame first so the modal visually floats above the
    // recessed main view.
    if app.overlay.is_active() {
        widgets::dim_background(frame, area);
    }
    // The `Help`/`Activity`/`Search`/`Compose` renderers take `&mut App`
    // (they clamp scroll offsets against the computed viewport), so they can't
    // run while holding an immutable borrow of `app.overlay`. Dispatch those
    // via a discriminant check; the payload-carrying overlays render by ref.
    match &app.overlay {
        Overlay::None
        | Overlay::Help
        | Overlay::Activity
        | Overlay::Search
        | Overlay::Compose(_) => {}
        Overlay::Confirm(dialog) => overlays::render_confirm_dialog(dialog, frame, area),
        Overlay::Attachment(picker) => overlays::render_attachment_picker(picker, frame, area),
        Overlay::Account(picker) => {
            overlays::render_account_picker(picker, &app.accounts, app.active_account, frame, area)
        }
        Overlay::Dir(picker) => overlays::render_dir_picker(picker, frame, area),
        Overlay::Mailbox(picker) => overlays::render_mailbox_picker(picker, frame, area),
        Overlay::Signatures(overlay) => overlays::render_signatures_overlay(overlay, frame, area),
        Overlay::Rsvp(overlay) => overlays::render_rsvp_overlay(overlay, frame, area),
        Overlay::Thread(overlay) => overlays::render_thread_overlay(overlay, frame, area),
        Overlay::Palette(palette) => overlays::render_command_palette(palette, frame, area),
        Overlay::Error(error) => overlays::render_persistent_error(error, frame, area),
    }
    if matches!(app.overlay, Overlay::Help) {
        overlays::render_help_overlay(app, frame, area);
    } else if matches!(app.overlay, Overlay::Activity) {
        activity::render_activity_overlay(app, frame, area);
    } else if matches!(app.overlay, Overlay::Search) {
        search::render_search_overlay(app, frame, area);
    } else if matches!(app.overlay, Overlay::Compose(_)) {
        compose::render_compose_wizard(app, frame, area);
    }

    // which-key popup (#0092): when a mnemonic family leader is pending and no
    // modal overlay owns the screen, float the pending family's continuations
    // (key + label) so the chord is discoverable while it is being typed. The
    // hint bar shows the same set inline; this is the larger, centred surface.
    if !app.overlay.is_active() && app.pending_prefix().is_some() {
        render_prefix_popup(app, frame, area);
    }
}

/// Draw the which-key popup for the pending family leader (#0092).
fn render_prefix_popup(app: &App, frame: &mut Frame, area: ratatui::layout::Rect) {
    use ratatui::layout::{Constraint, Direction, Flex, Layout};
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

    let Some(p) = app.pending_prefix() else {
        return;
    };
    let rows = app.prefix_popup_rows();
    if rows.is_empty() {
        return;
    }

    let theme = super::theme::active();
    let key_w = rows
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(3)
        .max(3);
    let inner_w = rows
        .iter()
        .map(|(k, d)| key_w.max(k.chars().count()) + 2 + d.chars().count())
        .max()
        .unwrap_or(12);
    let title = format!(" {} ", App::prefix_family_name(p));

    let width = ((inner_w as u16) + 4)
        .max(title.chars().count() as u16 + 4)
        .min(area.width.saturating_sub(2));
    let height = (rows.len() as u16 + 2).min(area.height.saturating_sub(2));

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(width)])
        .flex(Flex::Center)
        .split(area);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(height)])
        .flex(Flex::Center)
        .split(horizontal[0]);
    let popup = vertical[0];
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines: Vec<Line> = rows
        .iter()
        .map(|(keys, desc)| {
            Line::from(vec![
                Span::styled(
                    format!(" {keys:<key_w$}"),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {desc}"), Style::default().fg(theme.text)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    //! The outer frame (painted themes only): where it is drawn, what stays
    //! inside it, and the small-terminal tiers it costs two columns of.

    use ratatui::backend::TestBackend;
    use ratatui::buffer::{Buffer, Cell};
    use ratatui::style::Modifier;
    use ratatui::Terminal;

    use super::golden_frames::{self as gf, render_buffer};
    use super::{has_outer_frame, render_outer_frame};
    use crate::app::App;
    use crate::theme::{self, Theme};

    /// Every cell of the buffer's perimeter, paired with the glyph the frame
    /// puts there.
    fn perimeter(area: ratatui::layout::Rect) -> Vec<((u16, u16), &'static str)> {
        let (right, bottom) = (area.width - 1, area.height - 1);
        let mut cells = vec![
            ((0, 0), "╭"),
            ((right, 0), "╮"),
            ((0, bottom), "╰"),
            ((right, bottom), "╯"),
        ];
        for x in 1..right {
            cells.push(((x, 0), "─"));
            cells.push(((x, bottom), "─"));
        }
        for y in 1..bottom {
            cells.push(((0, y), "│"));
            cells.push(((right, y), "│"));
        }
        cells
    }

    /// Assert the perimeter of `buffer` is the outer frame, undimmed, in the
    /// active theme's border colour on its ground.
    fn assert_framed(buffer: &Buffer, what: &str) {
        let theme = theme::active();
        for ((x, y), glyph) in perimeter(*buffer.area()) {
            let cell = &buffer[(x, y)];
            assert_eq!(cell.symbol(), glyph, "{what}: frame glyph at ({x},{y})");
            assert_eq!(cell.fg, theme.border, "{what}: frame colour at ({x},{y})");
            assert_eq!(cell.bg, theme.bg, "{what}: frame ground at ({x},{y})");
            assert!(
                !cell.modifier.contains(Modifier::DIM),
                "{what}: an overlay dimmed the frame at ({x},{y})"
            );
        }
    }

    fn row_text(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area().width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn only_the_terminal_theme_goes_without_a_frame() {
        for name in theme::THEME_NAMES {
            let theme = Theme::by_name(name).unwrap();
            assert_eq!(has_outer_frame(&theme), *name != "terminal", "theme {name}");
        }
    }

    #[test]
    fn a_painted_theme_draws_the_four_rounded_corners() {
        let mut app = gf::mail_fixture();
        let buffer = render_buffer(&mut app, gf::WIDTH, gf::HEIGHT);
        assert_framed(&buffer, "mail view");
        // The panes moved inside: the sidebar's own corner and title now sit
        // one cell in, and the hint bar closes one row above the frame.
        assert_eq!(buffer[(1, 1)].symbol(), "╭");
        assert!(row_text(&buffer, 1).contains(" Mail "));
        assert_eq!(buffer[(1, gf::HEIGHT - 2)].symbol(), "╰");
        assert_eq!(buffer[(gf::WIDTH - 2, gf::HEIGHT - 2)].symbol(), "╯");
    }

    #[test]
    fn the_terminal_theme_draws_no_frame_and_keeps_the_whole_area() {
        let terminal_theme = Theme::terminal();
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut inner = None;
        terminal
            .draw(|frame| {
                inner = Some(render_outer_frame(frame, frame.area(), &terminal_theme));
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(inner, Some(*buffer.area()), "no frame, no lost cells");
        for ((x, y), _) in perimeter(*buffer.area()) {
            assert_eq!(
                buffer[(x, y)],
                Cell::default(),
                "nothing drawn at ({x},{y})"
            );
        }

        // A painted theme, on the same area, gives up one cell on each side.
        let painted = Theme::mailypoppins();
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| {
                inner = Some(render_outer_frame(frame, frame.area(), &painted));
            })
            .unwrap();
        assert_eq!(inner, Some(ratatui::layout::Rect::new(1, 1, 38, 10)));
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "╭");
        assert_eq!(terminal.backend().buffer()[(39, 11)].symbol(), "╯");
    }

    #[test]
    fn overlays_and_popups_stay_inside_the_frame() {
        type Fixture = fn() -> App;
        let fixtures: [(&str, Fixture); 6] = [
            ("help", gf::help_overlay_app),
            ("palette", gf::command_palette_app),
            ("account picker", gf::account_picker_app),
            ("signatures", gf::signatures_overlay_app),
            ("search", gf::search_form_filled_app),
            ("compose", gf::compose_wizard_with_body_app),
        ];
        // The full size and a cramped one, where a modal sized from the area
        // would reach the edge if it were given the whole terminal.
        for (width, height) in [(gf::WIDTH, gf::HEIGHT), (50, 14)] {
            for (name, fixture) in fixtures {
                let mut app = fixture();
                let buffer = render_buffer(&mut app, width, height);
                assert_framed(&buffer, &format!("{name} at {width}x{height}"));
            }
            // The which-key popup, which floats without a modal overlay.
            let mut app = gf::mail_fixture();
            app.pending_prefix = Some('g');
            let buffer = render_buffer(&mut app, width, height);
            assert_framed(&buffer, &format!("which-key at {width}x{height}"));
        }
    }

    /// The width tiers stay keyed on the terminal's own width, so an
    /// 80-column terminal keeps the full split inside the frame and 79 drops
    /// to the sidebar-and-list tier, as before the frame.
    #[test]
    fn the_width_tiers_survive_the_two_framed_columns() {
        let mut app = gf::mail_fixture();
        let wide = render_buffer(&mut app, 80, 24);
        assert_framed(&wide, "80x24");
        let wide_text: String = (0..24).map(|y| row_text(&wide, y)).collect();
        assert!(wide_text.contains(" Headers "), "80 columns: full split");

        let mut app = gf::mail_fixture();
        let medium = render_buffer(&mut app, 79, 24);
        assert_framed(&medium, "79x24");
        let medium_text: String = (0..24).map(|y| row_text(&medium, y)).collect();
        assert!(
            !medium_text.contains(" Headers "),
            "79 columns: no right column"
        );
        assert!(medium_text.contains(" Mail "), "79 columns: sidebar kept");

        let mut app = gf::mail_fixture();
        let narrow = render_buffer(&mut app, 39, 16);
        assert_framed(&narrow, "39x16");
        let narrow_text: String = (0..16).map(|y| row_text(&narrow, y)).collect();
        assert!(!narrow_text.contains(" Mail "), "39 columns: list alone");
        assert!(narrow_text.contains(" Inbox "), "39 columns: list kept");
    }

    /// No terminal is too small to paint: down to a single cell the frame and
    /// the layout inside it render without a panic, with and without a modal.
    #[test]
    fn tiny_terminals_render_without_panicking() {
        for width in 1..=8 {
            for height in 1..=8 {
                let mut app = gf::mail_fixture();
                let _ = render_buffer(&mut app, width, height);
                let mut app = gf::help_overlay_app();
                let _ = render_buffer(&mut app, width, height);
            }
        }
        // At three cells a side the frame is whole and the inside is one cell.
        let mut app = gf::mail_fixture();
        let buffer = render_buffer(&mut app, 3, 3);
        assert_eq!(buffer[(0, 0)].symbol(), "╭");
        assert_eq!(buffer[(2, 2)].symbol(), "╯");
    }
}
