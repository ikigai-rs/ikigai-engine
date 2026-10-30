//! A script of REPL lines (a `.iki` file, ledger #2485).
//!
//! A script is the lines a person would type at the REPL, run in file order, with three
//! additions:
//!
//! * **Comments.** A line whose first non-blank character is `#` is a comment, so a `#!`
//!   first line (a shebang) is one too. A `#` anywhere else keeps its meaning
//!   (`in=#243` still sends `#243`). [`Engine::eval`] skips such a line as well, so a file
//!   replayed through the shell (`ikigai --quiet < file`) reads its comments the same way.
//! * **Parameters.** `# @param <name>` or `# @param <name> = <default>` declares one, and a
//!   line uses it as `{name}`, the capture spelling of name templates. A name is an
//!   identifier (`^[A-Za-z_][A-Za-z0-9_-]*$`, the pattern of a plan's `ik:inputName`). An
//!   undeclared `{x}` is refused before any line runs, naming the line; a parameter with no
//!   default must be given; a parameter the script does not declare is refused by name.
//! * **Stop on error.** The run stops at the first line that fails and reports that line's
//!   number and error ([`ScriptError`]); a host exits non-zero on it.
//!
//! ⚠ Not yet a plan. The decided format compiles a script to an `ik:Process` so `plan`,
//! `urn:plan:validate` and `urn:plan:requires` accept it, but a plan cannot say "run these
//! lines in this order": `ik:step` is unordered by definition, the runner runs only what the
//! one `ik:result` depends on, and `ik:order` positions a fork's branches in its JOIN, not in
//! time. Until the process vocabulary has a term for ordered, independent steps, a script
//! runs line by line through the engine, and its parameters are substituted into the text.
//!
//! A value is substituted as ONE word: outside a `"…"` span it is quoted when it would
//! otherwise split (whitespace, an operator, a quote, or empty), and inside one its `"` and
//! `\` are escaped. So `in={who}` with `who=two words` sends `two words` to `in`.

use std::collections::BTreeMap;

use crate::engine::{Action, Engine};

/// One declared parameter: `# @param <name> [= <default>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    /// The parameter's name, what a line writes as `{name}`.
    pub name: String,
    /// Its value when the caller gives none. A parameter without one must be given.
    pub default: Option<String>,
}

/// A parsed script: its declared parameters and the lines that run, each with its 1-based
/// line number in the file. Comments are not among the lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Script {
    params: Vec<Param>,
    lines: Vec<(usize, String)>,
}

/// Why a script did not run to its end: the line (1-based) and that line's error. `line` is
/// `None` for a refusal of the caller's arguments, which belongs to no line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    /// The 1-based line number, or `None` for a refusal of the caller's arguments.
    pub line: Option<usize>,
    /// The error, as the line's own answer said it.
    pub message: String,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ScriptError {}

/// What running a script did: each line's [`Action`] in order, with its line number, and
/// the error that stopped it, if one did. The failing line's own action is the last one.
pub struct ScriptRun {
    /// Each line that ran, with its line number and what the engine answered.
    pub actions: Vec<(usize, Action)>,
    /// What stopped the run before its end, if anything did.
    pub error: Option<ScriptError>,
}

impl ScriptRun {
    /// Whether every line ran without error: a host's exit status.
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }
}

/// Whether `line` is a comment: its first non-blank character is `#`.
pub fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn at(line: usize, message: impl Into<String>) -> ScriptError {
    ScriptError {
        line: Some(line),
        message: message.into(),
    }
}

impl Script {
    /// Parse a script: collect its `@param` declarations and its lines, and refuse a
    /// malformed declaration or a `{name}` no declaration names, citing the line.
    pub fn parse(text: &str) -> Result<Script, ScriptError> {
        let mut params: Vec<Param> = Vec::new();
        let mut lines = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            if !is_comment(line) {
                lines.push((number, line.to_string()));
                continue;
            }
            let body = line.trim_start()[1..].trim();
            let Some(declaration) = body.strip_prefix("@param") else {
                continue;
            };
            if !declaration.is_empty() && !declaration.starts_with(char::is_whitespace) {
                // `# @parameters …` is prose, not a declaration.
                continue;
            }
            let param = parse_param(declaration).map_err(|message| at(number, message))?;
            if params.iter().any(|p| p.name == param.name) {
                return Err(at(
                    number,
                    format!("`@param {}` is declared twice", param.name),
                ));
            }
            params.push(param);
        }
        let script = Script { params, lines };
        for (number, line) in &script.lines {
            for name in captures(line) {
                if !script.params.iter().any(|p| p.name == name) {
                    return Err(at(
                        *number,
                        format!(
                            "`{{{name}}}` names no declared parameter (declare it with \
                             `# @param {name}`; the script declares {})",
                            script.declared()
                        ),
                    ));
                }
            }
        }
        Ok(script)
    }

    /// The declared parameters, in declaration order.
    pub fn params(&self) -> &[Param] {
        &self.params
    }

    fn declared(&self) -> String {
        if self.params.is_empty() {
            "none".to_string()
        } else {
            let names: Vec<&str> = self.params.iter().map(|p| p.name.as_str()).collect();
            names.join(", ")
        }
    }

    /// The lines to run, each with its line number, parameters substituted from `supplied`
    /// or their defaults. Refuses a supplied name the script does not declare, and a
    /// parameter with no default that `supplied` leaves out.
    pub fn bind(
        &self,
        supplied: &BTreeMap<String, String>,
    ) -> Result<Vec<(usize, String)>, ScriptError> {
        let refuse = |message: String| ScriptError {
            line: None,
            message,
        };
        if let Some(unknown) = supplied
            .keys()
            .find(|name| !self.params.iter().any(|p| &p.name == *name))
        {
            return Err(refuse(format!(
                "the script declares no parameter `{unknown}` (it declares {})",
                self.declared()
            )));
        }
        let mut values = BTreeMap::new();
        for param in &self.params {
            let value = supplied
                .get(&param.name)
                .or(param.default.as_ref())
                .ok_or_else(|| {
                    refuse(format!(
                        "the script's parameter `{0}` has no default; give it as \
                         `{0}=<value>`",
                        param.name
                    ))
                })?;
            values.insert(param.name.as_str(), value.as_str());
        }
        Ok(self
            .lines
            .iter()
            .map(|(number, line)| (*number, substitute(line, &values)))
            .collect())
    }
}

/// The text after `@param`: `<name>` or `<name> = <default>`.
fn parse_param(declaration: &str) -> Result<Param, String> {
    let declaration = declaration.trim();
    let (name, default) = match declaration.split_once('=') {
        Some((name, default)) => (name.trim(), Some(default.trim())),
        None => (declaration, None),
    };
    if name.is_empty() {
        return Err(
            "`@param` needs a name: `# @param <name>` or `# @param <name> = <default>`".into(),
        );
    }
    if !is_identifier(name) {
        return Err(format!(
            "`@param {name}`: a parameter name is an identifier (a letter or `_`, then \
             letters, digits, `_` or `-`)"
        ));
    }
    let default = default.map(unquote).transpose()?;
    Ok(Param {
        name: name.to_string(),
        default,
    })
}

/// A default written `"…"` is the text inside, with the engine's `\"` and `\\` escapes;
/// any other default is the text as written.
fn unquote(default: &str) -> Result<String, String> {
    let Some(inner) = default
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return Ok(default.to_string());
    };
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(e @ ('"' | '\\')) => out.push(e),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => return Err(format!("unterminated `\\` escape in `{default}`")),
            },
            '"' => return Err(format!("an unescaped `\"` inside the default `{default}`")),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// Each `{name}` in `line` whose inside is an identifier, in order. A brace around anything
/// else (`{"a":1}`, `{}`) is not a capture and is left alone.
fn captures(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    scan(line, |piece| {
        if let Piece::Capture(name) = piece {
            found.push(name.to_string());
        }
    });
    found
}

enum Piece<'a> {
    /// Text copied as it is.
    Text(&'a str),
    /// A `{name}`, and whether it sits inside a `"…"` span.
    Capture(&'a str),
    QuotedCapture(&'a str),
}

/// Walk `line` into text and captures, tracking the engine's `"…"` quoting (with its `\"`
/// and `\\` escapes) so a substitution knows which spelling keeps the value one word.
fn scan<'a>(line: &'a str, mut each: impl FnMut(Piece<'a>)) {
    let bytes = line.as_bytes();
    let mut quoted = false;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if quoted => i += 2,
            b'"' => {
                quoted = !quoted;
                i += 1;
            }
            b'{' => {
                let close = line[i + 1..].find('}').map(|offset| i + 1 + offset);
                match close {
                    Some(close) if is_identifier(&line[i + 1..close]) => {
                        each(Piece::Text(&line[start..i]));
                        let name = &line[i + 1..close];
                        each(if quoted {
                            Piece::QuotedCapture(name)
                        } else {
                            Piece::Capture(name)
                        });
                        i = close + 1;
                        start = i;
                    }
                    _ => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    each(Piece::Text(&line[start.min(line.len())..]));
}

/// `line` with every `{name}` replaced by its value, spelled so it stays one word.
fn substitute(line: &str, values: &BTreeMap<&str, &str>) -> String {
    let mut out = String::with_capacity(line.len());
    scan(line, |piece| match piece {
        Piece::Text(text) => out.push_str(text),
        Piece::Capture(name) => {
            let value = values[name];
            if needs_quotes(value) {
                out.push('"');
                out.push_str(&escape(value));
                out.push('"');
            } else {
                out.push_str(value);
            }
        }
        Piece::QuotedCapture(name) => out.push_str(&escape(values[name])),
    });
    out
}

fn needs_quotes(value: &str) -> bool {
    value.is_empty()
        || value == ".."
        || value
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '|' | '(' | ')' | ';' | '\\'))
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

impl Engine {
    /// Run a script of REPL lines (see [`crate::script`]) with `supplied` for its declared
    /// parameters: parse it, refuse what is wrong with it before any line runs, then run its
    /// lines in file order and stop at the first that fails. A synchronous `block_on` over
    /// [`run_script_async`](Self::run_script_async), as [`eval`](Self::eval) is.
    pub fn run_script(&self, text: &str, supplied: &BTreeMap<String, String>) -> ScriptRun {
        futures::executor::block_on(self.run_script_async(text, supplied))
    }

    /// The async form of [`run_script`](Self::run_script), for a browser frontend.
    pub async fn run_script_async(
        &self,
        text: &str,
        supplied: &BTreeMap<String, String>,
    ) -> ScriptRun {
        let refused = |error| ScriptRun {
            actions: Vec::new(),
            error: Some(error),
        };
        let lines = match Script::parse(text).and_then(|script| script.bind(supplied)) {
            Ok(lines) => lines,
            Err(error) => return refused(error),
        };
        let mut actions = Vec::new();
        for (number, line) in lines {
            let action = self.eval_async(&line).await;
            let failed = match &action {
                Action::Output(entry) => entry.result.as_ref().err().cloned(),
                _ => None,
            };
            let quit = matches!(action, Action::Quit);
            actions.push((number, action));
            if let Some(message) = failed {
                return ScriptRun {
                    actions,
                    error: Some(at(number, message)),
                };
            }
            if quit {
                break;
            }
        }
        ScriptRun {
            actions,
            error: None,
        }
    }
}
