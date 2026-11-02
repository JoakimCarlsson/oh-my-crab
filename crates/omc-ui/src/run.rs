//! The terminal setup and event loop.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use omc_auth::Provider;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::message::Msg;
use crate::state::{App, Mode, Role};
use crate::{tasks, view};

/// Runs the TUI until the user quits.
///
/// # Errors
///
/// Returns terminal I/O errors from drawing or reading input.
pub async fn run() -> anyhow::Result<()> {
    let active = omc_auth::active_provider().unwrap_or(None);
    let mut app = App::new(active);
    let (tx, mut rx) = mpsc::unbounded_channel();
    spawn_input_reader(tx.clone());
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &tx, &mut rx).await;
    ratatui::restore();
    result
}

/// Forwards terminal events to `tx` from a dedicated thread.
fn spawn_input_reader(tx: UnboundedSender<Msg>) {
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if tx.send(Msg::Term(event)).is_err() {
                break;
            }
        }
    });
}

/// Draws and handles messages until `app.quit` is set.
///
/// # Errors
///
/// Returns terminal I/O errors from drawing.
async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    tx: &UnboundedSender<Msg>,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
) -> anyhow::Result<()> {
    while !app.quit {
        terminal.draw(|frame| view::draw(frame, app))?;
        let Some(msg) = rx.recv().await else { break };
        update(app, tx, msg);
    }
    Ok(())
}

/// Applies one message to the application state.
fn update(app: &mut App, tx: &UnboundedSender<Msg>, msg: Msg) {
    match msg {
        Msg::Term(Event::Key(key)) if key.kind == KeyEventKind::Press => on_key(app, tx, key),
        Msg::Term(_) => {}
        Msg::LoginUrl(url) => app.say(format!(
            "Opening browser to log in. If it does not open, visit: {url}"
        )),
        Msg::LoggedIn(provider) => {
            app.active = Some(provider);
            app.say(format!("Logged in with {}. Say hi.", provider.label()));
        }
        Msg::LoginFailed(error) => app.say(format!("Login failed: {error}")),
        Msg::Delta(delta) => app.append_reply(&delta),
        Msg::Done => app.busy = false,
        Msg::Failed(error) => {
            app.busy = false;
            app.say(format!("Error: {error}"));
        }
        Msg::ModelChosen(provider, model) => {
            app.say(format!("Using {model}. Change it with /model <id>."));
            app.models.entry(provider).or_insert(model);
        }
        Msg::ModelList(provider, list) => show_models(app, provider, &list),
        Msg::ModelListFailed(error) => app.say(format!("Could not list models: {error}")),
    }
}

/// Prints the models `provider` offers, marking the one in use.
fn show_models(app: &mut App, provider: Provider, list: &[String]) {
    let current = app.models.get(&provider);
    let lines: Vec<String> = list
        .iter()
        .map(|id| {
            let mark = if Some(id) == current { "*" } else { " " };
            format!("  {mark} {id}")
        })
        .collect();
    app.say(format!(
        "Models for {} (use /model <id>):\n{}",
        provider.label(),
        lines.join("\n")
    ));
}

/// Handles `/model` with no argument: shows the current model and lists the rest.
fn model_overview(app: &mut App, tx: &UnboundedSender<Msg>) {
    let Some(provider) = app.active else {
        app.say("Log in first with /login.");
        return;
    };
    let current = app
        .models
        .get(&provider)
        .map_or("provider default".to_string(), Clone::clone);
    app.say(format!(
        "Current model: {current}. Fetching available models..."
    ));
    tasks::start_model_list(tx.clone());
}

/// Handles `/model <id>`: uses `id` for the active provider from now on.
fn set_model(app: &mut App, id: &str) {
    let Some(provider) = app.active else {
        app.say("Log in first with /login.");
        return;
    };
    app.models.insert(provider, id.to_string());
    app.say(format!("Model set to {id}."));
}

/// Routes a key press according to the current mode.
fn on_key(app: &mut App, tx: &UnboundedSender<Msg>, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    match app.mode {
        Mode::Picker(selected) => on_picker_key(app, tx, key.code, selected),
        Mode::Chat => on_chat_key(app, tx, key.code),
    }
}

/// Handles a key press while the login picker is open.
fn on_picker_key(app: &mut App, tx: &UnboundedSender<Msg>, code: KeyCode, selected: usize) {
    let last = Provider::ALL.len() - 1;
    match code {
        KeyCode::Up => app.mode = Mode::Picker(selected.saturating_sub(1)),
        KeyCode::Down => app.mode = Mode::Picker((selected + 1).min(last)),
        KeyCode::Esc => app.mode = Mode::Chat,
        KeyCode::Enter => {
            app.mode = Mode::Chat;
            tasks::start_login(tx.clone(), Provider::ALL[selected]);
        }
        _ => {}
    }
}

/// Handles a key press while typing on the input line.
fn on_chat_key(app: &mut App, tx: &UnboundedSender<Msg>, code: KeyCode) {
    match code {
        KeyCode::Char(c) => app.input.push(c),
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Enter => submit(app, tx),
        _ => {}
    }
}

/// Executes a slash command or sends the typed line as a message.
fn submit(app: &mut App, tx: &UnboundedSender<Msg>) {
    let line = std::mem::take(&mut app.input).trim().to_string();
    match line.as_str() {
        "" => {}
        "/quit" => app.quit = true,
        "/login" => app.mode = Mode::Picker(0),
        "/model" => model_overview(app, tx),
        command if command.starts_with("/model ") => {
            set_model(app, command["/model ".len()..].trim())
        }
        command if command.starts_with('/') => app.say(format!("Unknown command: {command}")),
        _ if app.busy => app.say("Still waiting for the previous reply."),
        _ => {
            app.push(Role::User, line.clone());
            app.busy = true;
            tasks::start_chat(tx.clone(), line, app.models.clone());
        }
    }
}
