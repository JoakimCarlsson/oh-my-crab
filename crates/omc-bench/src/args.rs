//! What the command line asks of a benchmark run.

use std::path::PathBuf;
use std::time::Duration;

/// Usage text printed for `--help` and after a bad argument.
pub(crate) const USAGE: &str = "usage: crab-bench <--model|--claude-code|--command> ... [options]\n\n  --model <provider[/model]>  measure crab with this model, as a provider id and optionally a model id\n  --claude-code <model>       measure Claude Code with this model, as a baseline\n  --command <name>=<command>  measure another program as a baseline: the command, split at spaces, with {prompt} standing for what the model is asked\n                              (repeat any of the three to compare several)\n  --runs <n>                  how many times each task is run for each contender (3)\n  --jobs <n>                  how many runs go on at once (4)\n  --task <name>               run only this task; repeat it for several\n  --tasks <folder>            the folder the tasks are read from (the bundled tasks)\n  --out <folder>              where the results, transcripts and edited files are written (build/bench/<time>)\n  --crab <program>            the crab to measure (the one beside crab-bench, else the one on PATH)\n  --timeout <seconds>         how long one run may take (300)\n  --tools <a,b,c>             the tools the model is given (read,grep,glob,edit)\n  --set <setting>=<value>     a conversation setting, such as effort=high; repeat it for several\n  --list                      print the tasks and stop";

/// The tools the model is given unless `--tools` names others.
const TOOLS: &[&str] = &["read", "grep", "glob", "edit"];

/// A model to measure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Model {
    /// The id of the provider that serves it.
    pub(crate) provider: String,
    /// The model's id, or nothing for the provider's default.
    pub(crate) model: Option<String>,
}

impl Model {
    /// The model `spec` names: a provider id, optionally followed by `/` and a model id, which may itself hold slashes.
    ///
    /// # Errors
    ///
    /// Fails when the provider id or the model id is empty.
    pub(crate) fn parse(spec: &str) -> Result<Self, String> {
        let (provider, model) = match spec.split_once('/') {
            Some((provider, model)) => (provider, Some(model)),
            None => (spec, None),
        };
        if provider.is_empty() || model.is_some_and(str::is_empty) {
            return Err(format!(
                "a model is written provider or provider/model, not {spec:?}"
            ));
        }
        Ok(Self {
            provider: provider.to_owned(),
            model: model.map(str::to_owned),
        })
    }

    /// The name the model goes by in the results, as it was written on the command line.
    pub(crate) fn label(&self) -> String {
        match &self.model {
            Some(model) => format!("{}/{model}", self.provider),
            None => self.provider.clone(),
        }
    }
}

/// The placeholder in a baseline command that stands for what the model is asked.
pub(crate) const PROMPT: &str = "{prompt}";

/// What is measured: crab with a model, or a baseline to hold it against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Contender {
    /// Crab, with this model.
    Crab(Model),
    /// Claude Code, with the model of this id.
    ClaudeCode(String),
    /// Another program, started once for each run.
    Command {
        /// The name it goes by in the results.
        name: String,
        /// The program and its arguments, one of which is [`PROMPT`].
        words: Vec<String>,
    },
}

impl Contender {
    /// The baseline program `spec` names, written `name=command`, the command split at white space.
    ///
    /// # Errors
    ///
    /// Fails when the name or the command is empty, or the command has no [`PROMPT`] word.
    pub(crate) fn command(spec: &str) -> Result<Self, String> {
        let (name, command) = spec.split_once('=').unwrap_or((spec, ""));
        let words: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
        if name.trim().is_empty() || words.is_empty() {
            return Err(format!("--command takes name=command, not {spec:?}"));
        }
        if !words.iter().any(|word| word == PROMPT) {
            return Err(format!(
                "the command of {name} needs {PROMPT} as a word of its own, where the prompt goes"
            ));
        }
        Ok(Self::Command {
            name: name.trim().to_owned(),
            words,
        })
    }

    /// The name the contender goes by in the results.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Crab(model) => format!("crab {}", model.label()),
            Self::ClaudeCode(model) => format!("claude-code {model}"),
            Self::Command { name, .. } => name.clone(),
        }
    }
}

/// Everything a benchmark run is told.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Options {
    /// What is measured, in the order it was named.
    pub(crate) contenders: Vec<Contender>,
    /// How many times each task is run for each contender.
    pub(crate) runs: usize,
    /// How many runs go on at once.
    pub(crate) jobs: usize,
    /// The names of the tasks to run, or every task when there are none.
    pub(crate) only: Vec<String>,
    /// The folder the tasks are read from, or the bundled one when there is none.
    pub(crate) tasks: Option<PathBuf>,
    /// The folder the results are written to, or one made for this run when there is none.
    pub(crate) out: Option<PathBuf>,
    /// The crab to measure, or the nearest one when there is none.
    pub(crate) crab: Option<PathBuf>,
    /// How long one run may take.
    pub(crate) timeout: Duration,
    /// The tools the model is given.
    pub(crate) tools: Vec<String>,
    /// The conversation settings every run starts with, as ids and values.
    pub(crate) settings: Vec<(String, String)>,
    /// Whether to print the tasks and stop.
    pub(crate) list: bool,
}

impl Default for Options {
    /// The options of a command line that names nothing.
    fn default() -> Self {
        Self {
            contenders: Vec::new(),
            runs: 3,
            jobs: 4,
            only: Vec::new(),
            tasks: None,
            out: None,
            crab: None,
            timeout: Duration::from_secs(300),
            tools: TOOLS.iter().map(|&tool| tool.to_owned()).collect(),
            settings: Vec::new(),
            list: false,
        }
    }
}

/// What the command line came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Asked {
    /// A benchmark run, or a listing of its tasks.
    Run(Box<Options>),
    /// The usage text.
    Help,
}

/// Reads the command line `arguments`, the program's name left out.
///
/// # Errors
///
/// Fails with what is wrong when an option is unknown, lacks its value or has one that does not read, and when nothing to measure is named for a run.
pub(crate) fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Asked, String> {
    let mut options = Options::default();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .ok_or_else(|| format!("{argument} needs a value"))
        };
        match argument.as_str() {
            "-h" | "--help" => return Ok(Asked::Help),
            "--list" => options.list = true,
            "--model" => options
                .contenders
                .push(Contender::Crab(Model::parse(&value()?)?)),
            "--claude-code" => options.contenders.push(Contender::ClaudeCode(value()?)),
            "--command" => options.contenders.push(Contender::command(&value()?)?),
            "--runs" => options.runs = count(&argument, &value()?)?,
            "--jobs" => options.jobs = count(&argument, &value()?)?,
            "--task" => options.only.push(value()?),
            "--tasks" => options.tasks = Some(PathBuf::from(value()?)),
            "--out" => options.out = Some(PathBuf::from(value()?)),
            "--crab" => options.crab = Some(PathBuf::from(value()?)),
            "--timeout" => {
                options.timeout = Duration::from_secs(count(&argument, &value()?)? as u64);
            }
            "--tools" => options.tools = listed(&value()?),
            "--set" => options.settings.push(setting(&value()?)?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    if options.contenders.is_empty() && !options.list {
        return Err(
            "name something to measure with --model, --claude-code or --command".to_owned(),
        );
    }
    Ok(Asked::Run(Box::new(options)))
}

/// The whole number from 1 that `text` gives the option `name`.
///
/// # Errors
///
/// Fails when `text` is not a whole number or is 0.
fn count(name: &str, text: &str) -> Result<usize, String> {
    match text.parse::<usize>() {
        Ok(number) if number > 0 => Ok(number),
        _ => Err(format!("{name} takes a whole number from 1, not {text:?}")),
    }
}

/// The names in the comma-separated `text`, blanks left out.
fn listed(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The setting id and value `text` gives, written `id=value`.
///
/// # Errors
///
/// Fails when `text` has no `=` or nothing before it.
fn setting(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((id, value)) if !id.is_empty() => Ok((id.to_owned(), value.to_owned())),
        _ => Err(format!("--set takes setting=value, not {text:?}")),
    }
}

#[cfg(test)]
/// Tests for reading the command line.
mod tests {
    use super::*;

    /// Reads `line`, split at spaces, as a command line.
    fn read(line: &str) -> Result<Asked, String> {
        parse(line.split_whitespace().map(str::to_owned))
    }

    /// A model is a provider alone or a provider and a model id, whose own slashes are kept.
    #[test]
    fn a_model_is_a_provider_and_maybe_a_model() {
        assert_eq!(
            Model::parse("claude").unwrap(),
            Model {
                provider: "claude".to_owned(),
                model: None
            }
        );
        let routed = Model::parse("openrouter/vendor/some-model").unwrap();
        assert_eq!(routed.provider, "openrouter");
        assert_eq!(routed.model.as_deref(), Some("vendor/some-model"));
        assert_eq!(routed.label(), "openrouter/vendor/some-model");
        assert!(Model::parse("/x").is_err());
        assert!(Model::parse("claude/").is_err());
    }

    /// Every option is read, and what is left out keeps its default.
    #[test]
    fn options_are_read() {
        let Asked::Run(options) = read(
            "--model claude --model codex/gpt --runs 2 --jobs 1 --task one --timeout 60 --tools read,edit, --set effort=high",
        )
        .unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(options.contenders.len(), 2);
        assert_eq!(options.contenders[1].label(), "crab codex/gpt");
        assert_eq!(options.runs, 2);
        assert_eq!(options.jobs, 1);
        assert_eq!(options.only, ["one"]);
        assert_eq!(options.timeout, Duration::from_secs(60));
        assert_eq!(options.tools, ["read", "edit"]);
        assert_eq!(options.settings, [("effort".to_owned(), "high".to_owned())]);
        assert_eq!(options.out, None);
    }

    /// A baseline is Claude Code with a model, or a named command that says where the prompt goes.
    #[test]
    fn baselines_are_read() {
        let Asked::Run(options) = parse(
            [
                "--claude-code",
                "some-model",
                "--command",
                "other=tool -p {prompt} --fast",
            ]
            .map(str::to_owned),
        )
        .unwrap() else {
            panic!("expected a run");
        };
        assert_eq!(options.contenders[0].label(), "claude-code some-model");
        assert_eq!(
            options.contenders[1],
            Contender::Command {
                name: "other".to_owned(),
                words: ["tool", "-p", "{prompt}", "--fast"]
                    .map(str::to_owned)
                    .to_vec(),
            }
        );
        assert!(Contender::command("other=tool -p").is_err());
        assert!(Contender::command("=tool {prompt}").is_err());
        assert!(Contender::command("other").is_err());
    }

    /// A run without a model is refused, a listing needs none, and bad values say what is wrong.
    #[test]
    fn mistakes_are_refused() {
        assert!(read("--runs 2").is_err());
        assert!(matches!(read("--list"), Ok(Asked::Run(_))));
        assert_eq!(read("--help"), Ok(Asked::Help));
        assert!(read("--model claude --runs 0").is_err());
        assert!(read("--model claude --runs").is_err());
        assert!(read("--model claude --set effort").is_err());
        assert!(read("--model claude --what").is_err());
    }
}
