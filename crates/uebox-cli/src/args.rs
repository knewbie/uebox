use std::{collections::HashMap, path::PathBuf, time::Duration};

use crate::{output::Fault, ReadTool, MAX_ASK_QUESTION_BYTES};

pub(crate) const DEFAULT_TIMEOUT_SECONDS: u64 = 120;

pub(crate) enum Command {
    Help,
    Version,
    Doctor {
        project: Option<PathBuf>,
    },
    Projects,
    ToolsList {
        search: Option<String>,
    },
    ToolsShow(ReadTool),
    ToolsCall {
        tool: ReadTool,
        project: Option<PathBuf>,
        input: Arguments,
    },
    Ask {
        model_profile_id: String,
        project: Option<PathBuf>,
        question: String,
    },
}

pub(crate) enum Arguments {
    Empty,
    Inline(String),
    File(PathBuf),
}

pub(crate) struct Options {
    pub command: Command,
    pub endpoint: Option<String>,
    pub timeout: Duration,
    pub json: bool,
    pub english: bool,
}

pub(crate) fn parse(argv: &[String]) -> Result<Options, Fault> {
    let mut words = Vec::new();
    let mut values = HashMap::new();
    let (mut help, mut version, mut json) = (false, false, false);
    let mut iter = argv.iter().map(String::as_str);
    while let Some(arg) = iter.next() {
        match arg {
            "--" => {
                words.extend(iter);
                break;
            }
            "--help" | "-h" => help = true,
            "--version" | "-v" => version = true,
            "--json" => json = true,
            "--allow-write" => return Err(Fault::RiskNotSupported),
            arg if arg.starts_with("--") => {
                let (flag, inline) = arg
                    .split_once('=')
                    .map_or((arg, None), |(key, value)| (key, Some(value)));
                if flag == "--allow-write" {
                    return Err(Fault::RiskNotSupported);
                }
                if !matches!(
                    flag,
                    "--endpoint"
                        | "--timeout"
                        | "--lang"
                        | "--project"
                        | "--search"
                        | "--name"
                        | "--model"
                        | "--args"
                        | "--args-file"
                ) {
                    return Err(Fault::InvalidArgument);
                }
                let value = inline
                    .or_else(|| iter.next().filter(|next| !next.starts_with("--")))
                    .filter(|value| !value.is_empty())
                    .ok_or(Fault::InvalidArgument)?;
                if values.insert(flag, value).is_some() {
                    return Err(Fault::InvalidArgument);
                }
            }
            arg if arg.starts_with('-') => return Err(Fault::InvalidArgument),
            _ => words.push(arg),
        }
    }

    let endpoint = values.remove("--endpoint").map(str::to_owned);
    if endpoint.as_deref().is_some_and(|value| {
        value.len() > 4096 || value.trim().is_empty() || value.chars().any(char::is_control)
    }) {
        return Err(Fault::InvalidArgument);
    }
    let seconds = match values.remove("--timeout") {
        Some(value) => value
            .parse::<u64>()
            .ok()
            .filter(|secs| (1..=3600).contains(secs))
            .ok_or(Fault::InvalidArgument)?,
        None => DEFAULT_TIMEOUT_SECONDS,
    };
    let english = match values.remove("--lang") {
        None | Some("zh-CN") => false,
        Some("en-US") => true,
        _ => return Err(Fault::InvalidArgument),
    };
    let command = if version {
        Command::Version
    } else if help || words.is_empty() {
        Command::Help
    } else {
        let project = values.remove("--project").map(PathBuf::from);
        let search = values.remove("--search").map(str::to_owned);
        let named = values.remove("--name");
        let model = values.remove("--model");
        let input = match (values.remove("--args"), values.remove("--args-file")) {
            (Some(_), Some(_)) => return Err(Fault::InvalidArgument),
            (Some(json), None) => Arguments::Inline(json.into()),
            (None, Some(path)) => Arguments::File(path.into()),
            (None, None) => Arguments::Empty,
        };
        let empty = matches!(input, Arguments::Empty);
        match words.as_slice() {
            ["help"]
                if project.is_none()
                    && search.is_none()
                    && named.is_none()
                    && model.is_none()
                    && empty =>
            {
                Command::Help
            }
            ["version"]
                if project.is_none()
                    && search.is_none()
                    && named.is_none()
                    && model.is_none()
                    && empty =>
            {
                Command::Version
            }
            ["doctor"] if search.is_none() && named.is_none() && model.is_none() && empty => {
                Command::Doctor { project }
            }
            ["projects", "list"]
                if project.is_none()
                    && search.is_none()
                    && named.is_none()
                    && model.is_none()
                    && empty =>
            {
                Command::Projects
            }
            ["tools", "list"]
                if project.is_none() && named.is_none() && model.is_none() && empty =>
            {
                Command::ToolsList { search }
            }
            ["tools", action @ ("show" | "call"), rest @ ..] if search.is_none() => {
                let name = match (rest, named) {
                    ([name], None) => *name,
                    ([], Some(name)) => name,
                    _ => return Err(Fault::InvalidArgument),
                };
                let tool = ReadTool::from_name(name).ok_or(Fault::ToolUnavailable)?;
                match *action {
                    "show" if project.is_none() && model.is_none() && empty => {
                        Command::ToolsShow(tool)
                    }
                    "call" if model.is_none() => Command::ToolsCall {
                        tool,
                        project,
                        input,
                    },
                    _ => return Err(Fault::InvalidArgument),
                }
            }
            ["ask", prompt @ ..] if search.is_none() && named.is_none() && empty => {
                let model_profile_id = model.ok_or(Fault::InvalidArgument)?;
                if !valid_model_profile_id(model_profile_id)
                    || prompt.is_empty()
                    || prompt.join(" ").trim().is_empty()
                    || prompt.join(" ").len() > MAX_ASK_QUESTION_BYTES
                {
                    return Err(Fault::InvalidArgument);
                }
                Command::Ask {
                    model_profile_id: model_profile_id.to_owned(),
                    project,
                    question: prompt.join(" "),
                }
            }
            _ => return Err(Fault::InvalidArgument),
        }
    };
    Ok(Options {
        command,
        endpoint,
        timeout: Duration::from_secs(seconds),
        json,
        english,
    })
}

pub(crate) fn help(english: bool) -> String {
    let intro = if english {
        "Unreal Box read-only console. --endpoint accepts the private socket directory on macOS and Linux or the local named pipe on Windows."
    } else {
        "虚幻盒子只读控制台。--endpoint 在 macOS 和 Linux 上指定私有套接字目录，在 Windows 上指定本机命名管道。"
    };
    format!("{intro}\n\nuebox [--json] [--endpoint <endpoint>] [--timeout <seconds>]\n  --help | --version\n  doctor [--project <path>]\n  projects list\n  tools list [--search <text>]\n  tools show <name>\n  tools call <name> [--project <path>] [--args '{{}}' | --args-file <path|->]\n  ask <question> --model <registered-profile-id> [--project <path>]\n\n--lang zh-CN|en-US\n")
}

fn valid_model_profile_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
