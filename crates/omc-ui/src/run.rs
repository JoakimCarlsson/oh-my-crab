//! The terminal setup and event loop.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::sync::Arc;

use omc_agent::{AgentEvent, Approvals, Decision};
use omc_auth::Provider;
use omc_tools::Registry;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::message::Msg;
use crate::state::{App, ApprovalPrompt, CHOICES, Mode, ModelPicker, Role};
use crate::tasks::UiPrompter;
use crate::{tasks, view};

/// Settings chosen on the command line.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Run `bash`, `edit` and `write` without asking.
    pub yolo: bool,
}

/// Runs the TUI until the user quits.
///
/// # Errors
///
/// Returns terminal I/O errors from drawing or reading input.
pub async fn run(options: Options) -> anyhow::Result<()> {
    let active = omc_auth::active_provider().unwrap_or(None);
    let registry = Registry::builtin(std::env::current_dir()?)?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let approvals = Approvals::new(Arc::new(UiPrompter::new(tx.clone())), options.yolo);
    let mut app = App::new(active, Arc::new(registry), Arc::new(approvals));
    if options.yolo {
        app.say("--yolo: bash, edit and write run without asking.");
    }
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
        Msg::Agent(event) => on_agent_event(app, event),
        Msg::Approval { request, reply } => {
            app.mode = Mode::Approve(ApprovalPrompt {
                request,
                reply: Some(reply),
                selected: 0,
            });
        }
        Msg::Done(history) => {
            app.history = history;
            app.busy = false;
        }
        Msg::Failed(error) => {
            app.busy = false;
            app.say(format!("Error: {error}"));
        }
        Msg::ModelChosen(provider, model) => {
            app.say(format!("Using {model}. Change it with /model <id>."));
            app.models.entry(provider).or_insert(model);
        }
        Msg::ModelList(provider, list) => fill_model_picker(app, provider, list),
        Msg::ModelListFailed(error) => {
            if matches!(app.mode, Mode::Models(_)) {
                app.mode = Mode::Chat;
            }
            app.say(format!("Could not list models: {error}"));
        }
    }
}

/// Shows one event of the running turn in the chat view.
fn on_agent_event(app: &mut App, event: AgentEvent) {
    match event {
        AgentEvent::Delta(delta) => app.append_reply(&delta),
        AgentEvent::ToolStart { name, summary, .. } => {
            app.push(Role::Tool, format!("{name} {summary}"));
        }
        AgentEvent::ToolDone { text, is_error, .. } => app.push_tool_result(&text, is_error),
    }
}

/// Opens the model picker and starts fetching the models to fill it.
fn open_model_picker(app: &mut App, tx: &UnboundedSender<Msg>) {
    if app.active.is_none() {
        app.say("Log in first with /login.");
        return;
    }
    app.mode = Mode::Models(ModelPicker {
        items: None,
        selected: 0,
    });
    tasks::start_model_list(tx.clone());
}

/// Fills the open model picker with `list`, highlighting the model in use.
fn fill_model_picker(app: &mut App, provider: Provider, list: Vec<String>) {
    let current = app.models.get(&provider);
    let selected = list.iter().position(|id| Some(id) == current).unwrap_or(0);
    if let Mode::Models(picker) = &mut app.mode {
        picker.items = Some(list);
        picker.selected = selected;
    }
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
        Mode::Models(_) => on_models_key(app, key.code),
        Mode::Approve(_) => on_approve_key(app, key.code),
        Mode::Chat => on_chat_key(app, tx, key.code),
    }
}

/// Handles a key press while a tool call awaits approval.
///
/// `y`, `a` and `n` answer directly, the arrows and Enter pick a highlighted
/// choice, and Esc denies.
fn on_approve_key(app: &mut App, code: KeyCode) {
    let Mode::Approve(prompt) = &mut app.mode else {
        return;
    };
    let decision = match code {
        KeyCode::Char('y') => Some(Decision::AllowOnce),
        KeyCode::Char('a') => Some(Decision::AllowAlways),
        KeyCode::Char('n') | KeyCode::Esc => Some(Decision::Deny),
        KeyCode::Up => {
            prompt.selected = prompt.selected.saturating_sub(1);
            None
        }
        KeyCode::Down => {
            prompt.selected = (prompt.selected + 1).min(CHOICES.len() - 1);
            None
        }
        KeyCode::Enter => Some(CHOICES[prompt.selected].1),
        _ => None,
    };
    let Some(decision) = decision else { return };
    if let Some(reply) = prompt.reply.take() {
        let _ = reply.send(decision);
    }
    app.mode = Mode::Chat;
}

/// Handles a key press while the model picker is open.
fn on_models_key(app: &mut App, code: KeyCode) {
    let Mode::Models(picker) = &mut app.mode else {
        return;
    };
    let count = picker.items.as_ref().map_or(0, Vec::len);
    match code {
        KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
        KeyCode::Down => picker.selected = (picker.selected + 1).min(count.saturating_sub(1)),
        KeyCode::Esc => app.mode = Mode::Chat,
        KeyCode::Enter => {
            let chosen = picker
                .items
                .as_ref()
                .and_then(|items| items.get(picker.selected))
                .cloned();
            app.mode = Mode::Chat;
            if let Some(id) = chosen {
                set_model(app, &id);
            }
        }
        _ => {}
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
        "/model" => open_model_picker(app, tx),
        command if command.starts_with("/model ") => {
            set_model(app, command["/model ".len()..].trim())
        }
        command if command.starts_with('/') => app.say(format!("Unknown command: {command}")),
        _ if app.busy => app.say("Still waiting for the previous reply."),
        _ => {
            app.push(Role::User, line.clone());
            app.busy = true;
            tasks::start_chat(
                tx.clone(),
                line,
                app.models.clone(),
                app.history.clone(),
                app.registry.clone(),
                app.approvals.clone(),
            );
        }
    }
}
