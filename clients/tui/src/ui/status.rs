use std::borrow::Cow;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use super::super::app::{
    hint_bindings, prefix_continuations, App, KeyAction, KeyCtx, Overlay, View,
};
use super::super::theme;
use super::util::display_width;

/// Rows the hint bar occupies: one content row inside a top and bottom
/// border, so it reads as a pane like every other one.
pub(super) const HINT_BAR_HEIGHT: u16 = 3;

/// A `keys` + label pair of the hint bar; a label is owned when it is not a
/// `KEYMAP` row's own (the event card's, PERSO-270).
type Hint = (&'static str, Cow<'static, str>);

/// herdr-style mode/hint bar (#0032): a single line showing an accent-bg mode
/// badge plus the next valid keystrokes for the current context, all derived
/// from `KEYMAP`. When a leader prefix (today `g`) is pending it shows that
/// prefix's continuations instead, making the previously-invisible leader
/// discoverable (Space -> the `m/c/a` view switch; `g` -> `gg`/`G`). The event
/// card's keys depend on its event, so they come from the card rather than
/// `KEYMAP` (PERSO-270). Overlay contexts with their own inline chips
/// (confirm, pickers, compose, ...) render no hint bar (`key_context()`
/// returns `None`).
pub(super) fn render_hint_bar(app: &App, frame: &mut Frame, area: Rect) {
    let bg = theme::active().bg;
    // The pane shell: same rounded border and background as the sidebar, list
    // and preview panes, so the bottom of the screen stops reading as a
    // foreign slab of colour.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::active().border))
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The profile the TUI runs on (PERSO-176), in every context, so the demo
    // profile cannot pass for the real mailbox while a dialog is open either.
    let profile = app.profile.as_ref().map(|name| {
        let text = format!("profile {name} ");
        let width = display_width(&text) + 1;
        let spans = [
            Span::styled(
                text,
                Style::default()
                    .fg(theme::active().warning)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default().bg(bg)),
        ];
        (spans, width)
    });

    // Only the profile, or an empty frame, when there is nothing contextual
    // to show, so the pane height stays stable.
    let Some(ctx) = app.key_context() else {
        if let Some((spans, _)) = profile {
            let bar = Paragraph::new(Line::from(spans.to_vec())).style(Style::default().bg(bg));
            frame.render_widget(bar, inner);
        }
        return;
    };

    // A reading pane (List / Headers / Body) shares the promoted MESSAGE
    // actions (#0092), so the hint bar merges the pane's own keys with the
    // message set; otherwise the bar would advertise only `j/k` and `v`.
    let msg_pane = matches!(ctx, KeyCtx::List | KeyCtx::Headers | KeyCtx::Preview);
    let pending = app.pending_prefix();
    let (badge, hints): (String, Vec<Hint>) = if let Some(p) = pending {
        // Leader pending: show its continuations (Space -> `m/c/a`; `g` ->
        // `gg`, `G`). Global leader continuations (the Space `m/c/a` view
        // switch, #0033) resolve before the pane context, so surface them
        // alongside the pane's own continuations whenever we are not already
        // in Global.
        let mut conts: Vec<(&str, Cow<str>)> = prefix_continuations(ctx, p)
            .map(|kb| (kb.keys, kb.hint_label().into()))
            .collect();
        if ctx != KeyCtx::Global {
            for kb in prefix_continuations(KeyCtx::Global, p) {
                conts.push((kb.keys, kb.hint_label().into()));
            }
        }
        if msg_pane {
            for kb in prefix_continuations(KeyCtx::Message, p) {
                if !conts.iter().any(|(k, _)| *k == kb.keys) {
                    conts.push((kb.keys, kb.hint_label().into()));
                }
            }
        }
        // The leader badge: a printable name for Space, otherwise the
        // uppercased key (e.g. `g` -> `G`).
        let badge = if p == ' ' {
            "SPACE".to_string()
        } else {
            p.to_uppercase().to_string()
        };
        (badge, conts)
    } else if let Overlay::EventCard(card) = &app.overlay {
        // The card's footer keys, capitalised like the `KEYMAP` hints.
        let hs = card
            .key_hints()
            .into_iter()
            .map(|(keys, label)| (keys, Cow::Owned(capitalised(label))))
            .collect();
        (mode_label(app, ctx), hs)
    } else {
        // Off-Mail, only the view-agnostic Global bindings actually fire
        // (mail-specific Global keys are swallowed by the dispatcher, #0033).
        // Filter the Global hint row to match so we don't advertise swallowed
        // keys (`1-9 Jump to mailbox`, `/ Filter by metadata`) in Contacts /
        // Calendar. Pane contexts (Contacts list) are unaffected.
        let off_mail = app.view != View::Mail;
        // The reminder badge takes the room of the Calendar view's `a/d/w
        // Layout` pair, so `c Calendars` and `n New` still fit a 100-column
        // terminal; the help overlay keeps the layout keys.
        let crowded = ctx == KeyCtx::Calendar && !app.reminders.is_empty();
        let mut hs: Vec<(&str, Cow<str>)> = hint_bindings(ctx)
            .filter(|kb| !(off_mail && ctx == KeyCtx::Global && !kb.action.is_view_agnostic()))
            .filter(|kb| !(crowded && kb.action == KeyAction::CalendarAgenda))
            .map(|kb| (kb.keys, kb.hint_label().into()))
            .collect();
        if msg_pane {
            for kb in hint_bindings(KeyCtx::Message) {
                if !hs.iter().any(|(k, _)| *k == kb.keys) {
                    hs.push((kb.keys, kb.hint_label().into()));
                }
            }
        }
        (mode_label(app, ctx), hs)
    };

    let badge_text = format!(" {} ", badge);
    let mut spans: Vec<Span> = Vec::with_capacity(hints.len() * 4 + 3);
    // Mode badge: bold, accent background, contrasting fg.
    let mut used = display_width(&badge_text) + 2;
    spans.push(Span::styled(
        badge_text,
        Style::default()
            .fg(theme::active().bg)
            .bg(theme::active().accent)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled("  ", Style::default().bg(bg)));
    if let Some((profile_spans, width)) = profile {
        used += width;
        spans.extend(profile_spans);
    }
    // The waiting calendar reminders (PERSO-255), from every view: the count
    // and the key that opens them, ahead of the hints it may crowd out, so it
    // stays short and lets the clock glyph say what the count is.
    if !app.reminders.is_empty() {
        let badge = format!("\u{25f7} {} (R) ", app.reminders.len());
        used += display_width(&badge) + 1;
        spans.push(Span::styled(
            badge,
            Style::default()
                .fg(theme::active().warning)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" ", Style::default().bg(bg)));
    }

    // Drop whole `keys` + label pairs that do not fit and mark the cut with an
    // ellipsis, rather than letting ratatui clip the last one mid-word
    // (#0078). The help overlay (`?`) still lists every binding in full, so
    // nothing dropped here is unreachable.
    let total = usize::from(inner.width);
    let mut truncated = false;
    for (i, (keys, label)) in hints.iter().enumerate() {
        let sep = usize::from(i > 0) * 2;
        let width = sep + display_width(keys) + 1 + display_width(label);
        // The ellipsis needs a cell of its own unless this is the last pair.
        let reserve = if i + 1 == hints.len() { 0 } else { 2 };
        if used + width + reserve > total {
            truncated = true;
            break;
        }
        used += width;
        if i > 0 {
            spans.push(Span::styled("  ", Style::default().bg(bg)));
        }
        spans.push(Span::styled(
            *keys,
            Style::default()
                .fg(theme::active().accent)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" ", Style::default().bg(bg)));
        spans.push(Span::styled(
            label.clone(),
            Style::default().fg(theme::active().text_muted).bg(bg),
        ));
    }
    if truncated {
        spans.push(Span::styled(
            " …",
            Style::default().fg(theme::active().text_muted).bg(bg),
        ));
    }

    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(bg));
    frame.render_widget(bar, inner);
}

/// `label` with its first letter in upper case.
fn capitalised(label: &str) -> String {
    let mut chars = label.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The badge label for a non-prefixed context. A live selection takes over the
/// badge (herdr's `N SELECTED`) since that is the most useful mode cue.
fn mode_label(app: &App, ctx: KeyCtx) -> String {
    // A zoom hides the other panes, so the bar has to say so: the badge is the
    // only chrome left that can (#TKT-0044). It suffixes whatever the badge
    // would otherwise be, selection included, because both facts matter.
    let zoom = if app.zoomed_pane().is_some() {
        " ZOOM"
    } else {
        ""
    };
    if !app.selection.is_empty() {
        return format!("{} SELECTED{zoom}", app.selection.len());
    }
    let base = match ctx {
        KeyCtx::Global => "MAIL",
        // `key_context()` never returns Message (it reports the focused pane),
        // but the match must be exhaustive; a reading pane is still "MAIL".
        KeyCtx::Message => "MAIL",
        KeyCtx::Sidebar => "MAILBOXES",
        KeyCtx::List => "MAIL",
        KeyCtx::Headers => "HEADERS",
        KeyCtx::Preview => "BODY",
        KeyCtx::ServerSearch => "SEARCH",
        KeyCtx::Contacts => "CONTACTS",
        KeyCtx::Calendar => "CALENDAR",
        KeyCtx::EventCard => "EVENT",
        KeyCtx::Activity => "ACTIVITY",
        KeyCtx::Help => "HELP",
    };
    format!("{base}{zoom}")
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent};

    use super::super::golden_frames::{self as gf, render_buffer};
    use crate::app::{App, KeyCtx, Overlay};

    /// The hint bar's content row: one above its bottom border, which sits
    /// one above the outer frame.
    fn hint_row(app: &mut App) -> String {
        let buffer = render_buffer(app, gf::WIDTH, gf::HEIGHT);
        let y = gf::HEIGHT - 3;
        (0..gf::WIDTH).map(|x| buffer[(x, y)].symbol()).collect()
    }

    /// The Calendar view with the event card open on the cursor's event.
    fn card_app(cursor: usize, edit: impl FnOnce(&mut App)) -> App {
        let mut app = gf::calendar_fixture();
        app.calendar_view.list_index = cursor;
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.overlay, Overlay::EventCard(_)));
        edit(&mut app);
        app
    }

    fn card(app: &mut App) -> &mut crate::app::EventCardOverlay {
        let Overlay::EventCard(card) = &mut app.overlay else {
            panic!("the event card is open");
        };
        card
    }

    /// The hint bar names the profile the TUI runs on (PERSO-176), in every
    /// context, an overlay with no hints of its own included, and says
    /// nothing on the default profile.
    #[test]
    fn the_hint_bar_names_the_active_profile() {
        let mut app = gf::calendar_fixture();
        assert!(!hint_row(&mut app).contains("profile"), "default profile");

        app.profile = Some("demo".to_string());
        let row = hint_row(&mut app);
        assert!(row.contains("profile demo"), "{row}");

        app.overlay = Overlay::Confirm(crate::app::ConfirmDialog {
            title: "Archive 2 emails?".to_string(),
            detail: "2 selected emails".to_string(),
            action: crate::app::ConfirmAction::Archive,
        });
        assert_eq!(app.key_context(), None, "a dialog has no hints");
        let row = hint_row(&mut app);
        assert!(row.contains("profile demo"), "no hints: {row}");
    }

    /// The event card reports its own context (PERSO-270), so the hint bar
    /// names it and lists the keys the card takes in the event's state.
    #[test]
    fn the_event_card_reports_its_context_and_keys() {
        // A server event the user attends: answer, edit, remove.
        let mut app = card_app(0, |_| {});
        assert_eq!(app.key_context(), Some(KeyCtx::EventCard));
        let row = hint_row(&mut app);
        assert!(row.contains(" EVENT "), "badge: {row}");
        for hint in ["V Respond", "E Edit", "D Cancel or delete", "Esc Close"] {
            assert!(row.contains(hint), "{hint:?} missing: {row}");
        }
        assert!(!row.contains("p Use"), "no proposal to use: {row}");
        assert!(!row.contains("Tab "), "no proposal to pick: {row}");

        // The organizer with two proposed times: use one, pick the next.
        let mut app = card_app(0, |app| {
            let card = card(app);
            card.event.is_organizer = true;
            let mut second = card.event.attendees[1].clone();
            second.email = "bob@example.org".into();
            card.event.attendees.push(second);
        });
        let row = hint_row(&mut app);
        for hint in ["p Use this time", "Tab Next proposal", "E Edit", "D Cancel"] {
            assert!(row.contains(hint), "{hint:?} missing: {row}");
        }
        assert!(
            !row.contains("V Respond"),
            "the organizer answers nobody: {row}"
        );

        // An invitation mail's event: answer, open the mail or its `.ics`.
        let mut app = card_app(0, |app| {
            let source = gf::calendar_fixture()
                .calendar_view
                .events
                .iter()
                .find_map(|event| event.source.clone())
                .unwrap();
            card(app).event.source = Some(source);
        });
        let row = hint_row(&mut app);
        for hint in ["V Respond", "m Open the mail", "e Open the .ics"] {
            assert!(row.contains(hint), "{hint:?} missing: {row}");
        }
        assert!(
            !row.contains("E Edit"),
            "a mail's event is not edited: {row}"
        );
    }
}
