mod compose;
mod headers;
mod list;
mod overlays;
mod preview;
mod search;
mod sidebar;
mod status;
mod util;

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::Frame;

use super::app::App;

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

    if let Some(dialog) = &app.confirm_dialog {
        overlays::render_confirm_dialog(dialog, frame, area);
    }

    if let Some(picker) = &app.attachment_picker {
        overlays::render_attachment_picker(picker, frame, area);
    }

    if let Some(picker) = &app.dir_picker {
        overlays::render_dir_picker(picker, frame, area);
    }

    if app.show_help {
        overlays::render_help_overlay(app, frame, area);
    }

    if app.show_search_overlay {
        search::render_search_overlay(app, frame, area);
    }

    if app.compose_wizard.is_some() {
        compose::render_compose_wizard(app, frame, area);
    }

    if let Some(error) = &app.persistent_error {
        overlays::render_persistent_error(error, frame, area);
    }
}
