//! The hooks of one conversation, run at the points the agent loop asks about.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::future::join_all;
use omc_agent::{Batched, CompactTrigger, Compacting, Followed, Hooks, Permit, Screened};
use omc_providers::{ToolCall, ToolResult};
use serde_json::{Map, Value, json};

use crate::answer::{self, Given, Submitted};
use crate::config::{Hook, HookEvent};
use crate::matcher::{Matcher, claude_names};
use crate::run::run;

/// The permission mode hooks are told, in Claude Code's words: `default`, `plan` or `bypassPermissions`.
pub type ModeNow = dyn Fn() -> &'static str + Send + Sync;

/// What every hook of a conversation is told and run with.
pub struct Setting {
    /// The conversation's id, when it has one.
    pub session: Option<String>,
    /// The folder the conversation works in, which hooks run in.
    pub folder: PathBuf,
    /// The permission mode as it stands when a hook runs.
    pub mode: Arc<ModeNow>,
}

/// The hooks of a conversation, for the main agent or for one subagent.
#[derive(Clone)]
pub struct Engine {
    /// The hooks that apply.
    hooks: Arc<Vec<Hook>>,
    /// What they are run with.
    setting: Arc<Setting>,
    /// The subagent they run for, as its id and kind, or nothing for the main agent.
    agent: Option<(String, String)>,
}

impl Engine {
    /// The engine running `hooks` for the main agent with `setting`.
    pub fn new(hooks: Vec<Hook>, setting: Setting) -> Self {
        Self {
            hooks: Arc::new(hooks),
            setting: Arc::new(setting),
            agent: None,
        }
    }

    /// Whether any hook runs at `event`.
    pub fn has(&self, event: HookEvent) -> bool {
        self.hooks.iter().any(|hook| hook.event == event)
    }

    /// Whether there is no hook at all.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Runs the hooks of a prompt the reader submitted as `prompt`, and says whether it goes to the model and with what context.
    pub async fn prompt(&self, prompt: &str) -> Submitted {
        let chosen = self.chosen(HookEvent::UserPromptSubmit, &[]);
        if chosen.is_empty() {
            return Submitted::Go;
        }
        let given = self
            .fire(
                HookEvent::UserPromptSubmit,
                &chosen,
                |_| json!({ "prompt": prompt }),
            )
            .await;
        answer::prompt(&given)
    }

    /// Runs the `SessionStart` hooks whose matcher takes `source`, how the conversation started (`startup`, `resume` or `compact`), telling them the `model` when there is one, and returns the context they add: their plain text and `additionalContext`, in order.
    pub async fn session_start(&self, source: &str, model: Option<&str>) -> Option<String> {
        let chosen = self.chosen(HookEvent::SessionStart, &[source]);
        if chosen.is_empty() {
            return None;
        }
        let mut specific = json!({ "source": source });
        if let Some(model) = model {
            specific["model"] = json!(model);
        }
        let given = self
            .fire(HookEvent::SessionStart, &chosen, |_| specific.clone())
            .await;
        answer::started(&given, true)
    }

    /// Runs the `SessionEnd` hooks whose matcher takes `reason`, why the conversation ended (`prompt_input_exit`, `clear`, `resume` or `other`), and waits for them to end; they cannot keep it open.
    pub async fn session_end(&self, reason: &str) {
        let chosen = self.chosen(HookEvent::SessionEnd, &[reason]);
        if chosen.is_empty() {
            return;
        }
        let given = self
            .fire(
                HookEvent::SessionEnd,
                &chosen,
                |_| json!({ "reason": reason }),
            )
            .await;
        answer::heard(&given);
    }

    /// Runs the `Notification` hooks whose matcher takes `kind`, the kind of notification (`permission_prompt`, `elicitation_dialog` or `idle_prompt`), telling them `message` and `title`, and waits for them to end; what they answer changes nothing.
    pub async fn notification(&self, kind: &str, title: &str, message: &str) {
        let chosen = self.chosen(HookEvent::Notification, &[kind]);
        if chosen.is_empty() {
            return;
        }
        let given = self
            .fire(
                HookEvent::Notification,
                &chosen,
                |_| json!({ "message": message, "title": title, "notification_type": kind }),
            )
            .await;
        answer::heard(&given);
    }

    /// Runs the `StopFailure` hooks whose matcher takes `error`, the kind of error that ended the turn (`rate_limit`, `overloaded`, `authentication_failed`, `billing_error`, `invalid_request`, `model_not_found`, `server_error`, `max_output_tokens` or `unknown`), telling them `details` and the error text the reader is shown as `last_assistant_message`, and waits for them; what they answer changes nothing.
    pub async fn stop_failure(&self, error: &str, details: &str, shown: &str) {
        let chosen = self.chosen(HookEvent::StopFailure, &[error]);
        if chosen.is_empty() {
            return;
        }
        let given = self
            .fire(HookEvent::StopFailure, &chosen, |_| {
                json!({ "error": error, "error_details": details, "last_assistant_message": shown })
            })
            .await;
        answer::heard(&given);
    }

    /// Runs the `PreModelSwitch` hooks whose matcher takes `to`, the model the reader asked for in place of `from`, and says why the switch does not happen when they keep it from happening.
    pub async fn model_switch(&self, from: &str, to: &str) -> Option<String> {
        let chosen = self.chosen(HookEvent::PreModelSwitch, &[to]);
        if chosen.is_empty() {
            return None;
        }
        let given = self
            .fire(HookEvent::PreModelSwitch, &chosen, |_| {
                switch_fields(from, to)
            })
            .await;
        answer::model_switch(&given)
    }

    /// Runs the `PostModelSwitch` hooks whose matcher takes `to`, the model that took over from `from`, and returns the context they add for the next request: their plain text and `additionalContext`, in order.
    pub async fn model_switched(&self, from: &str, to: &str) -> Option<String> {
        let chosen = self.chosen(HookEvent::PostModelSwitch, &[to]);
        if chosen.is_empty() {
            return None;
        }
        let given = self
            .fire(HookEvent::PostModelSwitch, &chosen, |_| {
                switch_fields(from, to)
            })
            .await;
        answer::started(&given, true)
    }

    /// The hooks of `event` whose matcher takes one of `names`, each command once; at events whose matcher names nothing, every hook of the event.
    fn chosen(&self, event: HookEvent, names: &[&str]) -> Vec<&Hook> {
        let mut chosen: Vec<&Hook> = Vec::new();
        for hook in self.hooks.iter().filter(|hook| hook.event == event) {
            let takes = match event {
                _ if event.ignores_matcher() => true,
                _ => match Matcher::parse(&hook.matcher) {
                    Ok(matcher) => matcher.matches(names),
                    Err(reason) => {
                        eprintln!(
                            "hook skipped: {} ({}): {reason}",
                            hook.label(),
                            hook.file.display()
                        );
                        false
                    }
                },
            };
            let repeated = chosen
                .iter()
                .any(|seen| seen.command == hook.command && seen.plugin == hook.plugin);
            if takes && !repeated {
                chosen.push(hook);
            }
        }
        chosen
    }

    /// Runs `chosen`, the hooks of `event`, all at once, each given the common fields and what `specific` adds for it, and returns how each ended.
    async fn fire<'h>(
        &self,
        event: HookEvent,
        chosen: &[&'h Hook],
        specific: impl Fn(&Hook) -> Value,
    ) -> Vec<Given<'h>> {
        let runs = chosen.iter().map(|hook| {
            let mut payload = self.common(event);
            if let Value::Object(extra) = specific(hook) {
                payload.extend(extra);
            }
            let input = serde_json::to_vec(&Value::Object(payload)).unwrap_or_default();
            let env = self.environment(hook);
            async move {
                let outcome = run(
                    &hook.command,
                    &input,
                    &self.setting.folder,
                    &env,
                    hook.limit(),
                )
                .await;
                Given { hook, outcome }
            }
        });
        join_all(runs).await
    }

    /// The fields every hook of `event` is given: the conversation's id, the folder, the event's name and the permission mode, and the subagent's id and kind when it runs for one.
    fn common(&self, event: HookEvent) -> Map<String, Value> {
        let mut payload = Map::new();
        payload.insert(
            "session_id".into(),
            json!(self.setting.session.clone().unwrap_or_default()),
        );
        payload.insert("cwd".into(), json!(self.setting.folder));
        payload.insert("hook_event_name".into(), json!(event.name()));
        payload.insert("permission_mode".into(), json!((self.setting.mode)()));
        if let Some((id, kind)) = &self.agent {
            payload.insert("agent_id".into(), json!(id));
            payload.insert("agent_type".into(), json!(kind));
        }
        payload
    }

    /// The variables `hook` runs with besides the harness's own: the project folder, and the plugin's folder for a plugin's hook.
    fn environment(&self, hook: &Hook) -> Vec<(String, String)> {
        let mut env = vec![(
            "CLAUDE_PROJECT_DIR".to_owned(),
            self.setting.folder.display().to_string(),
        )];
        if let Some(plugin) = &hook.plugin {
            env.push((
                "CLAUDE_PLUGIN_ROOT".to_owned(),
                plugin.display().to_string(),
            ));
        }
        env
    }

    /// The fields about `call`, made with `arguments`, that `hook` is given: the tool's name, by Claude Code's name for a hook from Claude Code's files, its arguments, with `file_path` added for such a hook when they hold a `path`, and the call's id.
    fn about_call(&self, hook: &Hook, call: &ToolCall, arguments: &Value) -> Map<String, Value> {
        let mut about = Map::new();
        let (name, input) = if hook.claude {
            let name = claude_names(&call.name)
                .first()
                .copied()
                .unwrap_or(&call.name);
            (name, claude_input(arguments, &self.setting.folder))
        } else {
            (call.name.as_str(), arguments.clone())
        };
        about.insert("tool_name".into(), json!(name));
        about.insert("tool_input".into(), input);
        about.insert("tool_use_id".into(), json!(call.id));
        about
    }

    /// The names the tool `name` goes by for matchers: crab's own and Claude Code's.
    fn names(name: &str) -> Vec<&str> {
        let mut names = vec![name];
        names.extend(claude_names(name));
        names
    }
}

/// The fields a hook of a model switch from `from` to `to` is given, as the client's `Set` command asks for a model the way an SDK host does.
fn switch_fields(from: &str, to: &str) -> Value {
    json!({ "from_model": from, "to_model": to, "requested_model": to, "source": "sdk" })
}

/// `arguments` as a hook from Claude Code's files expects them: with `file_path` holding `path` made absolute against `folder`, when there is a `path` and no `file_path`.
fn claude_input(arguments: &Value, folder: &Path) -> Value {
    let mut input = arguments.clone();
    if let Value::Object(fields) = &mut input
        && !fields.contains_key("file_path")
        && let Some(path) = fields.get("path").and_then(Value::as_str)
    {
        let absolute = folder.join(path);
        fields.insert("file_path".into(), json!(absolute));
    }
    input
}

/// `rewritten`, arguments a hook gave for a call made with `original`, without the `file_path` the hook was given beside `path`: it is moved back to `path` when the hook left `path` out, and dropped otherwise.
fn crab_input(rewritten: Value, original: &Value) -> Value {
    let Value::Object(mut fields) = rewritten else {
        return rewritten;
    };
    if original.get("file_path").is_none()
        && original.get("path").is_some()
        && let Some(file_path) = fields.remove("file_path")
        && !fields.contains_key("path")
    {
        fields.insert("path".into(), file_path);
    }
    Value::Object(fields)
}

#[async_trait]
impl Hooks for Engine {
    /// Runs the `PreToolUse` hooks that match the tool and reads what they decided.
    async fn before_tool(&self, call: &ToolCall, arguments: &Value) -> Screened {
        let chosen = self.chosen(HookEvent::PreToolUse, &Self::names(&call.name));
        if chosen.is_empty() {
            return Screened::default();
        }
        let given = self
            .fire(HookEvent::PreToolUse, &chosen, |hook| {
                Value::Object(self.about_call(hook, call, arguments))
            })
            .await;
        let mut screened = answer::before_tool(&given);
        screened.arguments = screened
            .arguments
            .map(|rewritten| crab_input(rewritten, arguments));
        screened
    }

    /// Runs the `PostToolUse` hooks that match the tool, giving them its output, and returns the note they add, the output they put in its place and whether they stop the turn.
    async fn after_tool(&self, call: &ToolCall, arguments: &Value, output: &str) -> Followed {
        let chosen = self.chosen(HookEvent::PostToolUse, &Self::names(&call.name));
        if chosen.is_empty() {
            return Followed::default();
        }
        let given = self
            .fire(HookEvent::PostToolUse, &chosen, |hook| {
                let mut about = self.about_call(hook, call, arguments);
                about.insert(
                    "tool_response".into(),
                    json!({ "output": output, "success": true }),
                );
                Value::Object(about)
            })
            .await;
        answer::after_tool(&given)
    }

    /// Runs the `PostToolUseFailure` hooks that match the tool, giving them its `error`, how long it ran and that it was not interrupted, and returns the note they add and whether they stop the turn.
    async fn after_tool_failed(
        &self,
        call: &ToolCall,
        arguments: &Value,
        error: &str,
        took: Duration,
    ) -> Followed {
        let chosen = self.chosen(HookEvent::PostToolUseFailure, &Self::names(&call.name));
        if chosen.is_empty() {
            return Followed::default();
        }
        let millis = u64::try_from(took.as_millis()).unwrap_or(u64::MAX);
        let given = self
            .fire(HookEvent::PostToolUseFailure, &chosen, |hook| {
                let mut about = self.about_call(hook, call, arguments);
                about.insert("error".into(), json!(error));
                about.insert("is_interrupt".into(), json!(false));
                about.insert("duration_ms".into(), json!(millis));
                Value::Object(about)
            })
            .await;
        let followed = answer::after_tool(&given);
        Followed {
            output: None,
            ..followed
        }
    }

    /// Runs the `PermissionRequest` hooks that match the tool, giving them the call as `PreToolUse` hooks get it without its id, and reads what they decided in the reader's place.
    async fn permission_request(&self, call: &ToolCall, arguments: &Value) -> Permit {
        let chosen = self.chosen(HookEvent::PermissionRequest, &Self::names(&call.name));
        if chosen.is_empty() {
            return Permit::Pass;
        }
        let given = self
            .fire(HookEvent::PermissionRequest, &chosen, |hook| {
                let mut about = self.about_call(hook, call, arguments);
                about.remove("tool_use_id");
                Value::Object(about)
            })
            .await;
        match answer::permission(&given) {
            Permit::Allow {
                arguments: Some(rewritten),
                rules,
            } => Permit::Allow {
                arguments: Some(crab_input(rewritten, arguments)),
                rules,
            },
            other => other,
        }
    }

    /// Runs the `PostToolBatch` hooks, giving them every call of the batch with the text its result holds, and returns the note they add and whether they stop the turn.
    async fn after_batch(&self, calls: &[ToolCall], results: &[ToolResult]) -> Batched {
        let chosen = self.chosen(HookEvent::PostToolBatch, &[]);
        if chosen.is_empty() {
            return Batched::default();
        }
        let given = self
            .fire(HookEvent::PostToolBatch, &chosen, |hook| {
                let listed: Vec<Value> = calls
                    .iter()
                    .map(|call| {
                        let mut about = self.about_call(hook, call, &call.arguments);
                        let response = results
                            .iter()
                            .find(|result| result.call_id == call.id)
                            .map_or("", |result| result.text.as_str());
                        about.insert("tool_response".into(), json!(response));
                        Value::Object(about)
                    })
                    .collect();
                json!({ "tool_calls": listed })
            })
            .await;
        answer::batch(&given)
    }

    /// Runs the `PostCompact` hooks whose matcher takes the trigger's name, telling them the summary as `compact_summary`; what they answer changes nothing.
    async fn after_compact(&self, trigger: CompactTrigger, summary: &str) {
        let chosen = self.chosen(HookEvent::PostCompact, &[trigger.name()]);
        if chosen.is_empty() {
            return;
        }
        let given = self
            .fire(
                HookEvent::PostCompact,
                &chosen,
                |_| json!({ "trigger": trigger.name(), "compact_summary": summary }),
            )
            .await;
        answer::heard(&given);
    }

    /// Runs the `PreCompact` hooks whose matcher takes the trigger's name, telling them the reader's `focus` as `custom_instructions`, and says whether the compaction goes on and with what instructions.
    async fn before_compact(&self, trigger: CompactTrigger, focus: Option<&str>) -> Compacting {
        let chosen = self.chosen(HookEvent::PreCompact, &[trigger.name()]);
        if chosen.is_empty() {
            return Compacting::default();
        }
        let given = self
            .fire(
                HookEvent::PreCompact,
                &chosen,
                |_| json!({ "trigger": trigger.name(), "custom_instructions": focus }),
            )
            .await;
        answer::compacting(&given)
    }

    /// Runs the `SubagentStart` hooks that match the kind of the subagent these hooks run for, and returns the `additionalContext` they add; nothing for the main agent.
    async fn subagent_start(&self) -> Option<String> {
        let (_, kind) = self.agent.as_ref()?;
        let chosen = self.chosen(HookEvent::SubagentStart, &[kind.as_str()]);
        if chosen.is_empty() {
            return None;
        }
        let given = self
            .fire(HookEvent::SubagentStart, &chosen, |_| json!({}))
            .await;
        answer::started(&given, false)
    }

    /// Runs the `Stop` hooks, or the `SubagentStop` hooks that match a subagent's kind, and returns why the agent should go on.
    async fn stop(&self, answer: &str, again: bool) -> Option<String> {
        let (event, kind) = match &self.agent {
            Some((_, kind)) => (HookEvent::SubagentStop, vec![kind.as_str()]),
            None => (HookEvent::Stop, Vec::new()),
        };
        let chosen = self.chosen(event, &kind);
        if chosen.is_empty() {
            return None;
        }
        let given = self
            .fire(
                event,
                &chosen,
                |_| json!({ "stop_hook_active": again, "last_assistant_message": answer }),
            )
            .await;
        answer::stop(&given)
    }

    /// The same hooks, run for the subagent `id` of the kind `kind`.
    fn for_subagent(&self, id: &str, kind: &str) -> Arc<dyn Hooks> {
        Arc::new(Self {
            agent: Some((id.to_owned(), kind.to_owned())),
            ..self.clone()
        })
    }
}

#[cfg(all(test, unix))]
/// Tests for running a conversation's hooks.
mod tests {
    use std::time::Duration;

    use omc_agent::Gate;

    use super::*;
    use crate::config::HookOrigin;
    use crate::testutil::scratch;

    /// A hook for `event` matching `matcher` that runs `command`, from Claude Code's files when `claude` is set.
    fn hook(event: HookEvent, matcher: &str, command: &str, claude: bool) -> Hook {
        Hook {
            event,
            matcher: matcher.to_owned(),
            command: command.to_owned(),
            timeout: Some(Duration::from_secs(10)),
            file: PathBuf::from("/h.json"),
            origin: HookOrigin::User,
            claude,
            plugin: None,
        }
    }

    /// An engine running `hooks` in `folder`, in the default mode.
    fn engine(folder: &Path, hooks: Vec<Hook>) -> Engine {
        Engine::new(
            hooks,
            Setting {
                session: Some("c-1".into()),
                folder: folder.to_path_buf(),
                mode: Arc::new(|| "default"),
            },
        )
    }

    /// A call of `name` with `arguments`.
    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: name.into(),
            arguments,
        }
    }

    /// A Claude Code hook that denies `bash` commands holding `rm` reads the payload Claude Code would send, by the name `Bash`, and denies with its reason; other commands and other tools pass.
    #[tokio::test]
    async fn a_claude_code_guard_denies_rm() {
        let folder = scratch("engine-guard");
        let guard = r#"input=$(cat); if grep -q '"tool_name":"Bash"' <<<"$input" && grep -q '"command":"rm ' <<<"$input"; then echo "rm is not allowed here" >&2; exit 2; fi"#;
        let hooks = engine(
            &folder,
            vec![hook(HookEvent::PreToolUse, "Bash", guard, true)],
        );
        let screened = hooks
            .before_tool(
                &call("bash", json!({"command": "rm -rf build"})),
                &json!({"command": "rm -rf build"}),
            )
            .await;
        assert_eq!(screened.gate, Gate::Deny("rm is not allowed here".into()));
        let screened = hooks
            .before_tool(
                &call("bash", json!({"command": "ls"})),
                &json!({"command": "ls"}),
            )
            .await;
        assert_eq!(screened.gate, Gate::Pass);
        let screened = hooks
            .before_tool(
                &call("read", json!({"path": "rm x"})),
                &json!({"path": "rm x"}),
            )
            .await;
        assert_eq!(screened, Screened::default());
    }

    /// The payload carries the common fields, the subagent's identity, and for a Claude Code hook the tool by Claude Code's name with `file_path`; a rewrite of `file_path` comes back as `path`.
    #[tokio::test]
    async fn the_payload_and_a_rewrite() {
        let folder = scratch("engine-payload");
        let echo = r#"cat > payload.json; echo '{"hookSpecificOutput":{"permissionDecision":"allow","updatedInput":{"file_path":"other.txt","content":"x"}}}'"#;
        let hooks = engine(
            &folder,
            vec![hook(HookEvent::PreToolUse, "Write", echo, true)],
        );
        let sub = hooks.for_subagent("t9/2", "scout");
        let arguments = json!({"path": "a.txt", "content": "x"});
        let screened = sub
            .before_tool(&call("write", arguments.clone()), &arguments)
            .await;
        assert_eq!(screened.gate, Gate::Allow);
        assert_eq!(
            screened.arguments,
            Some(json!({"path": "other.txt", "content": "x"}))
        );
        let payload: Value =
            serde_json::from_str(&std::fs::read_to_string(folder.join("payload.json")).unwrap())
                .unwrap();
        assert_eq!(payload["hook_event_name"], "PreToolUse");
        assert_eq!(payload["session_id"], "c-1");
        assert_eq!(payload["permission_mode"], "default");
        assert_eq!(payload["cwd"], json!(folder));
        assert_eq!(payload["agent_id"], "t9/2");
        assert_eq!(payload["agent_type"], "scout");
        assert_eq!(payload["tool_name"], "Write");
        assert_eq!(payload["tool_use_id"], "t1");
        assert_eq!(
            payload["tool_input"]["file_path"],
            json!(folder.join("a.txt"))
        );
    }

    /// After a call the hook sees the output and its context becomes the note; at the end of a turn a hook asks to go on, and a subagent's end runs only the `SubagentStop` hooks matching its kind.
    #[tokio::test]
    async fn after_a_call_and_at_the_end() {
        let folder = scratch("engine-after");
        let note = r#"grep -q '"output":"alpha' && echo '{"hookSpecificOutput":{"additionalContext":"saw alpha"}}'"#;
        let hooks = engine(
            &folder,
            vec![
                hook(HookEvent::PostToolUse, "read", note, false),
                hook(
                    HookEvent::Stop,
                    "",
                    "echo 'run the tests' >&2; exit 2",
                    false,
                ),
                hook(
                    HookEvent::SubagentStop,
                    "reviewer",
                    "echo 'review again' >&2; exit 2",
                    false,
                ),
            ],
        );
        let arguments = json!({"path": "a.txt"});
        let added = hooks
            .after_tool(&call("read", arguments.clone()), &arguments, "alpha\n")
            .await;
        assert_eq!(added.note.as_deref(), Some("saw alpha"));
        assert_eq!(
            hooks.stop("done", false).await.as_deref(),
            Some("run the tests")
        );
        assert_eq!(
            hooks.for_subagent("t1", "scout").stop("done", false).await,
            None
        );
        assert_eq!(
            hooks
                .for_subagent("t1", "reviewer")
                .stop("done", false)
                .await
                .as_deref(),
            Some("review again")
        );
    }

    /// A prompt hook adds what it prints, a failing one is ignored, and the same command listed twice runs once.
    #[tokio::test]
    async fn a_prompt_gets_context() {
        let folder = scratch("engine-prompt");
        let hooks = engine(
            &folder,
            vec![
                hook(
                    HookEvent::UserPromptSubmit,
                    "",
                    "echo on branch main >> ran.txt; echo on branch main",
                    false,
                ),
                hook(
                    HookEvent::UserPromptSubmit,
                    "",
                    "echo on branch main >> ran.txt; echo on branch main",
                    false,
                ),
                hook(HookEvent::UserPromptSubmit, "", "exit 1", false),
            ],
        );
        assert_eq!(
            hooks.prompt("hello").await,
            Submitted::WithContext("on branch main".into())
        );
        assert_eq!(
            std::fs::read_to_string(folder.join("ran.txt")).unwrap(),
            "on branch main\n"
        );
    }

    /// The payload a hook wrote to `file` in `folder`, as JSON.
    fn written(folder: &Path, file: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(folder.join(file)).unwrap()).unwrap()
    }

    /// `SessionStart` runs only the hooks whose matcher takes the source, tells them the source and model, and hands back their plain text and `additionalContext`.
    #[tokio::test]
    async fn a_session_starts() {
        let folder = scratch("engine-start");
        let hooks = engine(
            &folder,
            vec![
                hook(
                    HookEvent::SessionStart,
                    "startup|resume",
                    "cat > start.json; echo the secret is kiwi",
                    true,
                ),
                hook(
                    HookEvent::SessionStart,
                    "compact",
                    r#"echo '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"compacted"}}'"#,
                    true,
                ),
            ],
        );
        assert_eq!(
            hooks.session_start("startup", Some("m-1")).await.as_deref(),
            Some("the secret is kiwi")
        );
        let payload = written(&folder, "start.json");
        assert_eq!(payload["hook_event_name"], "SessionStart");
        assert_eq!(payload["source"], "startup");
        assert_eq!(payload["model"], "m-1");
        assert_eq!(
            hooks.session_start("compact", None).await.as_deref(),
            Some("compacted")
        );
        assert_eq!(hooks.session_start("clear", None).await, None);
    }

    /// `SessionEnd` and `Notification` hooks are told the reason and the notification, by matcher, and a blocking exit changes nothing.
    #[tokio::test]
    async fn an_end_and_a_notification() {
        let folder = scratch("engine-end");
        let hooks = engine(
            &folder,
            vec![
                hook(
                    HookEvent::SessionEnd,
                    "",
                    "cat > end.json; echo bye >&2; exit 2",
                    false,
                ),
                hook(
                    HookEvent::Notification,
                    "permission_prompt",
                    "cat > asked.json",
                    false,
                ),
                hook(
                    HookEvent::Notification,
                    "idle_prompt",
                    "cat > idle.json",
                    false,
                ),
            ],
        );
        hooks.session_end("prompt_input_exit").await;
        assert_eq!(written(&folder, "end.json")["reason"], "prompt_input_exit");
        hooks
            .notification(
                "permission_prompt",
                "Permission needed",
                "crab needs your permission to use bash",
            )
            .await;
        let asked = written(&folder, "asked.json");
        assert_eq!(asked["hook_event_name"], "Notification");
        assert_eq!(asked["notification_type"], "permission_prompt");
        assert_eq!(asked["title"], "Permission needed");
        assert_eq!(asked["message"], "crab needs your permission to use bash");
        assert!(!folder.join("idle.json").exists());
    }

    /// `PostToolUseFailure` hooks see the error and how long the call ran, and their context becomes the note; `PreCompact` hooks see the trigger and the focus, and their plain text becomes instructions or exit code 2 blocks.
    #[tokio::test]
    async fn a_failure_and_a_compaction() {
        let folder = scratch("engine-failure");
        let hooks = engine(
            &folder,
            vec![
                hook(
                    HookEvent::PostToolUseFailure,
                    "Bash",
                    r#"cat > failed.json; echo '{"hookSpecificOutput":{"hookEventName":"PostToolUseFailure","additionalContext":"try make"}}'"#,
                    true,
                ),
                hook(
                    HookEvent::PreCompact,
                    "manual",
                    "cat > compact.json; echo keep the todo list",
                    false,
                ),
                hook(
                    HookEvent::PreCompact,
                    "auto",
                    "echo saving first >&2; exit 2",
                    false,
                ),
            ],
        );
        let arguments = json!({"command": "false"});
        let note = hooks
            .after_tool_failed(
                &call("bash", arguments.clone()),
                &arguments,
                "exit code 1",
                Duration::from_millis(42),
            )
            .await;
        assert_eq!(note.note.as_deref(), Some("try make"));
        let failed = written(&folder, "failed.json");
        assert_eq!(failed["hook_event_name"], "PostToolUseFailure");
        assert_eq!(failed["tool_name"], "Bash");
        assert_eq!(failed["error"], "exit code 1");
        assert_eq!(failed["is_interrupt"], false);
        assert_eq!(failed["duration_ms"], 42);
        assert_eq!(
            hooks
                .after_tool(&call("bash", arguments.clone()), &arguments, "ok")
                .await,
            Followed::default()
        );
        assert_eq!(
            hooks
                .before_compact(CompactTrigger::Manual, Some("the API"))
                .await,
            Compacting::Go(Some("keep the todo list".into()))
        );
        let compact = written(&folder, "compact.json");
        assert_eq!(compact["trigger"], "manual");
        assert_eq!(compact["custom_instructions"], "the API");
        assert_eq!(
            hooks.before_compact(CompactTrigger::Auto, None).await,
            Compacting::Blocked("saving first".into())
        );
    }

    /// `SubagentStart` runs for a subagent of a kind its matcher takes, with the subagent's identity, and hands back `additionalContext`; the main agent runs none.
    #[tokio::test]
    async fn a_subagent_starts() {
        let folder = scratch("engine-subagent");
        let hooks = engine(
            &folder,
            vec![hook(
                HookEvent::SubagentStart,
                "scout",
                r#"cat > sub.json; echo '{"hookSpecificOutput":{"hookEventName":"SubagentStart","additionalContext":"stay read-only"}}'"#,
                false,
            )],
        );
        assert_eq!(hooks.subagent_start().await, None);
        assert_eq!(
            hooks.for_subagent("t1", "reviewer").subagent_start().await,
            None
        );
        assert_eq!(
            hooks
                .for_subagent("t2", "scout")
                .subagent_start()
                .await
                .as_deref(),
            Some("stay read-only")
        );
        let payload = written(&folder, "sub.json");
        assert_eq!(payload["hook_event_name"], "SubagentStart");
        assert_eq!(payload["agent_id"], "t2");
        assert_eq!(payload["agent_type"], "scout");
    }

    /// A slow hook before a call times out and the call is denied, without the turn waiting past the timeout.
    #[tokio::test]
    async fn a_slow_guard_denies() {
        let folder = scratch("engine-slow");
        let mut slow = hook(HookEvent::PreToolUse, "*", "sleep 30", false);
        slow.timeout = Some(Duration::from_millis(200));
        let hooks = engine(&folder, vec![slow]);
        let started = std::time::Instant::now();
        let screened = hooks
            .before_tool(&call("read", json!({})), &json!({}))
            .await;
        assert!(matches!(screened.gate, Gate::Deny(reason) if reason.contains("gave no answer")));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// `PermissionRequest` hooks get the call without its id and decide for the reader; `PostToolBatch` hooks get every call with its result; `PostCompact`, `StopFailure` and the model switch hooks get their fields, and a `PreModelSwitch` hook can keep the switch from happening.
    #[tokio::test]
    async fn the_newer_events() {
        let folder = scratch("engine-newer");
        let hooks = engine(
            &folder,
            vec![
                hook(
                    HookEvent::PermissionRequest,
                    "Bash",
                    r#"cat > permission.json; echo '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'"#,
                    true,
                ),
                hook(
                    HookEvent::PostToolBatch,
                    "",
                    r#"cat > batch.json; echo '{"hookSpecificOutput":{"hookEventName":"PostToolBatch","additionalContext":"two calls"}}'"#,
                    false,
                ),
                hook(HookEvent::PostCompact, "auto", "cat > compact.json", false),
                hook(
                    HookEvent::StopFailure,
                    "rate_limit",
                    "cat > failure.json",
                    false,
                ),
                hook(
                    HookEvent::PreModelSwitch,
                    "*opus*",
                    "cat > switch.json; echo 'opus is too dear' >&2; exit 2",
                    false,
                ),
                hook(
                    HookEvent::PostModelSwitch,
                    "",
                    "echo now on a new model",
                    false,
                ),
            ],
        );
        let arguments = json!({"command": "ls"});
        let permit = hooks
            .permission_request(&call("bash", arguments.clone()), &arguments)
            .await;
        assert_eq!(
            permit,
            Permit::Allow {
                arguments: None,
                rules: Vec::new()
            }
        );
        let asked = written(&folder, "permission.json");
        assert_eq!(asked["hook_event_name"], "PermissionRequest");
        assert_eq!(asked["tool_name"], "Bash");
        assert!(asked.get("tool_use_id").is_none());
        let calls = vec![call("bash", arguments.clone())];
        let results = vec![ToolResult {
            call_id: "t1".into(),
            name: "bash".into(),
            text: "a.txt".into(),
            is_error: false,
            images: Vec::new(),
        }];
        let batched = hooks.after_batch(&calls, &results).await;
        assert_eq!(batched.note.as_deref(), Some("two calls"));
        let batch = written(&folder, "batch.json");
        assert_eq!(batch["tool_calls"][0]["tool_name"], "bash");
        assert_eq!(batch["tool_calls"][0]["tool_response"], "a.txt");
        hooks
            .after_compact(CompactTrigger::Auto, "we did things")
            .await;
        assert_eq!(
            written(&folder, "compact.json")["compact_summary"],
            "we did things"
        );
        hooks
            .stop_failure("rate_limit", "429 Too Many Requests", "rate limited")
            .await;
        let failure = written(&folder, "failure.json");
        assert_eq!(failure["error"], "rate_limit");
        assert_eq!(failure["error_details"], "429 Too Many Requests");
        assert_eq!(
            hooks
                .model_switch("claude-sonnet-5", "claude-opus-5")
                .await
                .as_deref(),
            Some("opus is too dear")
        );
        let switch = written(&folder, "switch.json");
        assert_eq!(switch["from_model"], "claude-sonnet-5");
        assert_eq!(switch["to_model"], "claude-opus-5");
        assert_eq!(hooks.model_switch("a", "claude-haiku-5").await, None);
        assert_eq!(
            hooks.model_switched("a", "b").await.as_deref(),
            Some("now on a new model")
        );
    }
}
