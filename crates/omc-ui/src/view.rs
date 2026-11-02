//! Rendering of the chat view, input line and login picker.

use omc_auth::Provider;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::state::{App, ApprovalPrompt, CHOICES, Mode, ModelPicker, Role};

/// Draws the whole interface into `frame`.
pub fn draw(frame: &mut Frame, app: &App) {
    let [chat, input] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());
    draw_chat(frame, app, chat);
    draw_input(frame, app, input);
    match &app.mode {
        Mode::Picker(selected) => draw_picker(frame, *selected),
        Mode::Models(picker) => draw_model_picker(frame, app, picker),
        Mode::Approve(prompt) => draw_approval(frame, prompt),
        Mode::Chat => {}
    }
}

/// Splits `text` into lines no wider than `width` terminal columns.
///
/// Width is measured in display columns, so wide characters count double and
/// zero-width ones count nothing.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let mut line = String::new();
        let mut columns = 0;
        for c in raw.chars() {
            let w = c.width().unwrap_or(0);
            if columns + w > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                columns = 0;
            }
            line.push(c);
            columns += w;
        }
        lines.push(line);
    }
    lines
}

/// Returns the style used for entries written by `role`.
fn role_style(role: Role) -> Style {
    match role {
        Role::User => Style::new().fg(Color::Cyan),
        Role::Assistant => Style::new().fg(Color::Green),
        Role::System => Style::new().fg(Color::DarkGray),
        Role::Tool => Style::new().fg(Color::Yellow),
        Role::ToolOutput => Style::new().fg(Color::DarkGray),
        Role::ToolError => Style::new().fg(Color::Red),
    }
}

/// Returns the prefix printed before the first line of an entry.
fn role_prefix(role: Role) -> &'static str {
    match role {
        Role::User => "you: ",
        Role::Assistant => "crab: ",
        Role::System => "* ",
        Role::Tool => "> ",
        Role::ToolOutput => "  ",
        Role::ToolError => "  ! ",
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

/// Draws the model picker centered over the chat, or a loading note until the
/// models have been fetched.
fn draw_model_picker(frame: &mut Frame, app: &App, picker: &ModelPicker) {
    let provider = app.active.map_or("provider", Provider::label);
    let title = format!(" Models for {provider} (Enter select, Esc cancel) ");
    let block = Block::bordered().title(title);
    let Some(items) = &picker.items else {
        let area = centered(frame.area(), 44, 3);
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new("Loading models...").block(block), area);
        return;
    };
    let current = app.active.and_then(|provider| app.models.get(&provider));
    let rows: Vec<ListItem> = items
        .iter()
        .map(|id| {
            let mark = if Some(id) == current { "*" } else { " " };
            ListItem::new(format!("{mark} {id}"))
        })
        .collect();
    let widest = items.iter().map(|id| id.chars().count()).max().unwrap_or(0);
    let area = centered(
        frame.area(),
        (widest as u16 + 6).max(44),
        items.len() as u16 + 2,
    );
    let list = List::new(rows)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(picker.selected));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

/// Most lines of the call description shown in the approval prompt.
const APPROVAL_LINES: usize = 20;

/// Draws the approval prompt centered over the chat: the call and the choices.
fn draw_approval(frame: &mut Frame, prompt: &ApprovalPrompt) {
    let width = frame.area().width.clamp(20, 80);
    let inner = width.saturating_sub(2) as usize;
    let wrapped = wrap(&prompt.request.summary, inner);
    let hidden = wrapped.len().saturating_sub(APPROVAL_LINES);
    let mut lines: Vec<Line> = wrapped
        .into_iter()
        .take(APPROVAL_LINES)
        .map(Line::from)
        .collect();
    if hidden > 0 {
        lines.push(Line::styled(
            format!("… {hidden} more lines not shown; deny if unsure"),
            Style::new().fg(Color::Yellow),
        ));
    }
    lines.push(Line::from(""));
    for (index, (label, _)) in CHOICES.iter().enumerate() {
        let style = if index == prompt.selected {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        lines.push(Line::styled(format!(" {label}"), style));
    }
    let title = format!(" Allow {}? ", prompt.request.tool);
    let area = centered(frame.area(), width, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        area,
    );
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

    /// Wide characters count as two columns.
    #[test]
    fn wrap_measures_display_width() {
        assert_eq!(wrap("日本語", 4), vec!["日本", "語"]);
    }
}
