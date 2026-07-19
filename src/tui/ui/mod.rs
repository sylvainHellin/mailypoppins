mod activity;
mod compose;
mod headers;
mod list;
mod overlays;
mod preview;
mod search;
mod sidebar;
mod status;
mod util;
mod widgets;

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::Frame;

use super::app::{App, Overlay};

/// Render the entire UI from the current app state.
pub fn view(app: &mut App, frame: &mut Frame) {
    let area = frame.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let main_area = outer[0];
    let status_area = outer[1];

    let show_right = app.terminal_width >= 80;
    let show_sidebar = app.terminal_width >= 40;

    if show_right {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(main_area);

        let left_col = columns[0];
        let right_col = columns[1];

        let sidebar_height = (app.mailboxes.len() as u16) + 3;
        let left_panels = if app.show_activity_log {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(sidebar_height),
                    Constraint::Min(0),
                    Constraint::Length(6),
                ])
                .split(left_col)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(sidebar_height), Constraint::Min(0)])
                .split(left_col)
        };

        sidebar::render_sidebar(app, frame, left_panels[0]);
        list::render_email_list(app, frame, left_panels[1]);
        if app.show_activity_log && left_panels.len() > 2 {
            sidebar::render_activity_log(app, frame, left_panels[2]);
        }

        let right_panels = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(sidebar_height), Constraint::Min(0)])
            .split(right_col);

        headers::render_headers(app, frame, right_panels[0]);
        preview::render_body(app, frame, right_panels[1]);
    } else if show_sidebar {
        let sidebar_height = (app.mailboxes.len() as u16) + 2;
        let left_panels = if app.show_activity_log {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(sidebar_height),
                    Constraint::Min(0),
                    Constraint::Length(6),
                ])
                .split(main_area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(sidebar_height), Constraint::Min(0)])
                .split(main_area)
        };

        sidebar::render_sidebar(app, frame, left_panels[0]);
        list::render_email_list(app, frame, left_panels[1]);
        if app.show_activity_log && left_panels.len() > 2 {
            sidebar::render_activity_log(app, frame, left_panels[2]);
        }
    } else {
        list::render_email_list(app, frame, main_area);
    }

    status::render_status_bar(app, frame, status_area);

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
        Overlay::None | Overlay::Help | Overlay::Activity | Overlay::Search
        | Overlay::Compose(_) => {}
        Overlay::Confirm(dialog) => overlays::render_confirm_dialog(dialog, frame, area),
        Overlay::Attachment(picker) => {
            overlays::render_attachment_picker(picker, frame, area)
        }
        Overlay::Dir(picker) => overlays::render_dir_picker(picker, frame, area),
        Overlay::Mailbox(picker) => overlays::render_mailbox_picker(picker, frame, area),
        Overlay::Rsvp(overlay) => overlays::render_rsvp_overlay(overlay, frame, area),
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
}
