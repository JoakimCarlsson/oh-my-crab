//! Rendering of the chat view, input line and login picker.

use omc_auth::Provider;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use crate::state::{App, Mode, Role};

/// Draws the whole interface into `frame`.
pub fn draw(frame: &mut Frame, app: &App) {
    let [chat, input] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());
    draw_chat(frame, app, chat);
    draw_input(frame, app, input);
    if let Mode::Picker(selected) = app.mode {
        draw_picker(frame, selected);
    }
}

/// Splits `text` into lines no wider than `width` characters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let chars: Vec<char> = raw.chars().collect();
        if chars.is_empty() {
            lines.push(String::new());
        }
        lines.extend(chars.chunks(width).map(|chunk| chunk.iter().collect()));
    }
    lines
}

/// Returns the style used for entries written by `role`.
fn role_style(role: Role) -> Style {
    match role {
        Role::User => Style::new().fg(Color::Cyan),
        Role::Assistant => Style::new().fg(Color::Green),
        Role::System => Style::new().fg(Color::DarkGray),
    }
}

/// Returns the prefix printed before the first line of an entry.
fn role_prefix(role: Role) -> &'static str {
    match role {
        Role::User => "you: ",
        Role::Assistant => "crab: ",
        Role::System => "* ",
    }
}

/// Draws the chat entries, pinned to the bottom of `area`.
fn draw_chat(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(" crab ");
    let inner = block.inner(area);
    let lines: Vec<Line> = app
        .entries
        .iter()
        .flat_map(|entry| {
            let text = format!("{}{}", role_prefix(entry.role), entry.text);
            wrap(&text, inner.width as usize)
                .into_iter()
                .map(move |line| Line::styled(line, role_style(entry.role)))
        })
        .collect();
    let skip = lines.len().saturating_sub(inner.height as usize);
    let visible: Vec<Line> = lines.into_iter().skip(skip).collect();
    frame.render_widget(Paragraph::new(visible).block(block), area);
}

/// Draws the one-line input and places the cursor after the typed text.
fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let prompt = if app.busy { "… " } else { "> " };
    let line = format!("{prompt}{}", app.input);
    let width = line.chars().count() as u16;
    frame.render_widget(Paragraph::new(line), area);
    if matches!(app.mode, Mode::Chat) {
        let x = area.x + width.min(area.width.saturating_sub(1));
        frame.set_cursor_position((x, area.y));
    }
}

/// Draws the login picker centered over the chat.
fn draw_picker(frame: &mut Frame, selected: usize) {
    let height = Provider::ALL.len() as u16 + 2;
    let area = centered(frame.area(), 44, height);
    let items: Vec<ListItem> = Provider::ALL
        .iter()
        .map(|provider| ListItem::new(provider.label()))
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(" Login (Enter select, Esc cancel) "))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

/// Returns a `width` by `height` rectangle centered in `area`, clamped to fit.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long lines are split at the width and blank lines survive.
    #[test]
    fn wrap_splits_long_lines_and_keeps_blank_ones() {
        assert_eq!(wrap("abcde\n\nf", 2), vec!["ab", "cd", "e", "", "f"]);
    }
}
