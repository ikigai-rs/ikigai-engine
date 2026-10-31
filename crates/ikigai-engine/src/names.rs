//! **The names text form** — a host's names atom written as lines (named requests E4, ledger
//! #2509).
//!
//! A request with arguments gets a NAME, bound in a host's names atom, instead of the kernel
//! parsing arguments out of IRIs (ledger #2447). Core holds a binding as a
//! [`TopologyRule`](ikigai_core::TopologyRule) (a named request: [`TopologyRule::named_request`](ikigai_core::TopologyRule::named_request) or
//! [`TopologyRule::named_template`](ikigai_core::TopologyRule::named_template)) and declares one in Turtle as an `ik:RewriteRule` with
//! `ik:fixes`, `ik:verb` and `ik:input`. This module is the form a person writes, one binding
//! per line, in the same words the REPL's `bind` command takes:
//!
//! ```text
//! # quakes
//! urn:quakes:count = source urn:sparql:select graph=urn:quakes:graph query=<urn:quakes:count.rq>
//! urn:quakes:event:{id} = source urn:iki:fn:compose src=<urn:quakes:event.tpl> id={id}
//! urn:quakes:report = source urn:plan:eval in=<urn:plans:report.ttl>
//!   input region summary="the region to report on" default=world
//! ```
//!
//! - **A line is `<name> = <verb> <target> [key=value …]`.** The verb is the one the name
//!   forwards (`source`, `sink`, `exists` or `delete`), or `forward` for every verb. Each
//!   `key=value` FIXES that argument of the target: the caller cannot give it.
//! - **A value is text, a reference or a capture.** An unquoted `key=<iri>` fixes a
//!   REFERENCE, as the REPL sends one (ledger #2508); `"…"` quotes a value (and keeps `<iri>`
//!   text). In a template name (`{var}` in it, RFC 6570 simple), `key={var}` fixes the WHOLE
//!   value to what the variable captured, and a `{var}` in the target fills the target IRI.
//!   An unquoted `key=<urn:file:searches/{var}>` fills a reference template, percent-encoding
//!   each capture before parsing the IRI. A capture is never spliced into longer text:
//!   an unquoted `key=a{id}` is refused.
//! - **An indented `input` line declares an input of the name itself**, under the binding
//!   above it: `input <name> [summary="…"] [class=<iri>] [default="…"] [one-of="…"]…
//!   [required=true|false]`, required unless it has a default. `bind` writes these for a
//!   plan binding, copying the plan's `ik:input`s, so the name has the plan's closed contract.
//! - **`#` starts a comment line.** Blank lines are ignored.
//!
//! **The order is the table's, not the author's.** Core's [`AliasTable`](ikigai_core::AliasTable) consults its rules
//! most specific first, whatever order they were added in, so a names file is read into that
//! order and printed in it. Two bindings of one name are refused, never shadowed.
//!
//! **The Turtle face** (the `plan-reader` feature, which carries the Turtle reader):
//! `names_to_turtle` and `turtle_to_names`, served by `space` as the transreptors
//! `urn:names:to-rdf` and `urn:names:from-rdf`. The Turtle is an `ik:Alias` over an empty
//! `ik:EndpointSpace` — what `urn:kernel:topology` renders for an alias holding these rules,
//! and what [`build`](ikigai_core::build) builds. Both are declared LOSSLESS, and it is
//! checked: each reads its own output back and refuses to answer unless it gets the bindings
//! it was given. A comment is the one thing the round trip does not keep.

use std::fmt;
use std::sync::Arc;

use ikigai_core::{
    Alias, AliasTable, ArgSpec, EndpointSpace, FixedArgument, FixedValue, ResolveAt, RuleKind,
    Space, SpaceKind, TopologyRule, UriTemplate, Verb,
};

use crate::engine::{looks_like_argument_name, tokenize, Token, Word};

/// The media type of a names file: `text/x-ikigai-names`.
pub const MEDIA_NAMES: &str = "text/x-ikigai-names";

/// A names file that could not be read, or a binding it cannot hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamesError {
    /// The 1-based line the refusal is about; 0 for the document as a whole.
    pub line: usize,
    /// What was wrong.
    pub detail: String,
}

impl NamesError {
    fn at(line: usize, detail: impl Into<String>) -> Self {
        NamesError {
            line,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for NamesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            0 => write!(f, "names file: {}", self.detail),
            line => write!(f, "names file line {line}: {}", self.detail),
        }
    }
}

impl std::error::Error for NamesError {}

const USAGE: &str = "a binding is `<name> = <verb> <target> [key=value …]`, the verb one of \
                     source, sink, exists, delete or forward";

/// Read ONE binding, `<name> = <verb> <target> [key=value …]` — the words of a names line and
/// of the REPL's `bind`. Refused, in core's words where core is the one refusing, when the
/// rule could not mean one thing (see [`AliasTable::with_rule`]).
///
/// ```
/// use ikigai_engine::names::parse_binding;
/// use ikigai_core::FixedValue;
///
/// let rule = parse_binding("urn:quakes:event:{id} = source urn:iki:fn:compose id={id}").unwrap();
/// assert!(rule.template);
/// assert_eq!(rule.fixes[0].value, FixedValue::Capture("id".into()));
/// assert!(parse_binding("urn:x:{id} = source urn:t id=a{id}").is_err());
/// ```
pub fn parse_binding(line: &str) -> Result<TopologyRule, String> {
    let words = words(line)?;
    let [name, eq, verb, target, args @ ..] = &words[..] else {
        return Err(format!("{USAGE}; got `{}`", line.trim()));
    };
    if eq.is_quoted() || &**eq != "=" {
        return Err(format!("{USAGE}; got `{}`", line.trim()));
    }
    let verb = match &**verb {
        "source" => Some(Verb::Source),
        "sink" => Some(Verb::Sink),
        "exists" => Some(Verb::Exists),
        "delete" => Some(Verb::Delete),
        "forward" => None,
        other => {
            return Err(format!(
                "`{other}` is not a verb a name forwards: source, sink, exists, delete, or \
                 forward for every verb"
            ))
        }
    };
    let template = name.contains('{');
    let mut rule = if template {
        TopologyRule::named_template(&**name, &**target)
    } else {
        TopologyRule::named_request(&**name, &**target)
    };
    if let Some(verb) = verb {
        rule = rule.verb(verb);
    }
    for arg in args {
        let Some((key, value)) = arg.split_once('=') else {
            return Err(format!(
                "`{arg}` is not `key=value`: a binding fixes its target's arguments by name"
            ));
        };
        if !looks_like_argument_name(key) {
            return Err(format!(
                "`{arg}` does not name an argument (a letter, then letters, digits, _ or -)"
            ));
        }
        if rule.fixes.iter().any(|fixed| fixed.name == key) {
            return Err(format!(
                "`{key}` is fixed twice: a binding gives each argument once"
            ));
        }
        rule = rule.fixing(fixed_argument(arg, key, value)?);
    }
    check(&rule)?;
    Ok(rule)
}

/// The words of a line, every one a word: an operator (`|`, `(`, `)`, `;`, `..`) is refused,
/// since a binding is one request.
fn words(line: &str) -> Result<Vec<Word>, String> {
    tokenize(line)?
        .into_iter()
        .map(|token| match token {
            Token::Word(word) => Ok(word),
            _ => Err(format!(
                "a binding names ONE request, so `|`, `..`, `(`, `)` and `;` have no place in \
                 it; quote a value that holds one: `{}`",
                line.trim()
            )),
        })
        .collect()
}

/// One `key=value` word as the argument it fixes.
fn fixed_argument(word: &Word, key: &str, value: &str) -> Result<FixedArgument, String> {
    if !word.is_quoted() {
        if let Some(inner) = value.strip_prefix('<').and_then(|v| v.strip_suffix('>')) {
            if inner.contains('{') || inner.contains('}') {
                let template = UriTemplate::parse(inner)
                    .map_err(|e| format!("`{key}={value}` is not a reference template: {e}"))?;
                return Ok(FixedArgument::reference_template(key, template));
            }
        }
    }
    if let Some(reference) = word.reference() {
        let (_, iri) = reference?;
        return Ok(FixedArgument::reference(key, iri));
    }
    if !word.is_quoted() && (value.contains('{') || value.contains('}')) {
        if let Some(var) = value
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
            .filter(|var| is_variable(var))
        {
            return Ok(FixedArgument::capture(key, var));
        }
        return Err(format!(
            "`{key}={value}`: a capture `{{var}}` fills a WHOLE value, never a part of a longer \
             one; quote the value to send the braces as text: {key}=\"{value}\""
        ));
    }
    Ok(FixedArgument::value(key, value))
}

/// An RFC 6570 variable name, as a capture spells it.
fn is_variable(var: &str) -> bool {
    !var.is_empty() && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Core's check of one rule, the one [`AliasTable::with_rule`] applies.
fn check(rule: &TopologyRule) -> Result<(), String> {
    AliasTable::new()
        .with_rule(rule.clone())
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Read one indented `input <name> [option=value …]` line.
fn parse_input(line: &str) -> Result<ArgSpec, String> {
    let words = words(line)?;
    let [head, name, options @ ..] = &words[..] else {
        return Err(format!(
            "an indented line declares an input: `input <name> [summary=\"…\"] [class=<iri>] \
             [default=\"…\"] [one-of=\"…\"]… [required=true|false]`; got `{}`",
            line.trim()
        ));
    };
    if &**head != "input" || head.is_quoted() {
        return Err(format!(
            "an indented line declares an input of the binding above it (`input <name> …`); \
             got `{}`",
            line.trim()
        ));
    }
    let mut input = ArgSpec::new(&**name);
    let mut required = None;
    let mut one_of: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for option in options {
        let Some((key, value)) = option.split_once('=') else {
            return Err(format!("`{option}` is not `option=value`"));
        };
        if key != "one-of" {
            if seen.contains(&key) {
                return Err(format!(
                    "`{key}` is given twice for the input `{}`",
                    &**name
                ));
            }
            seen.push(key);
        }
        match key {
            "summary" => input.summary = value.to_string(),
            "class" => {
                input.class = Some(match option.reference() {
                    Some(reference) => reference?.1.as_str().to_string(),
                    None => value.to_string(),
                })
            }
            "default" => input.default = Some(value.to_string()),
            "one-of" => one_of.push(value.to_string()),
            "required" => {
                required = Some(match value {
                    "true" => true,
                    "false" => false,
                    other => return Err(format!("`required={other}` is true or false")),
                })
            }
            other => {
                return Err(format!(
                    "`{other}` is not an input option: summary, class, default, one-of or \
                     required"
                ))
            }
        }
    }
    if !one_of.is_empty() {
        input = input.one_of(one_of);
    }
    input.required = required.unwrap_or(input.default.is_none());
    Ok(input)
}

/// Read a names file into its bindings, in TABLE order (most specific first, as core
/// consults them). Every binding is checked as core's [`AliasTable::with_rule`] checks it,
/// and a name bound twice is refused.
///
/// ```
/// use ikigai_engine::names::{parse_names, render_names};
///
/// let text = "urn:a = source urn:t:upper in=hello\n";
/// let rules = parse_names(text).unwrap();
/// assert_eq!(render_names(&rules).unwrap(), text);
/// ```
pub fn parse_names(text: &str) -> Result<Vec<TopologyRule>, NamesError> {
    let mut rules: Vec<(usize, TopologyRule)> = Vec::new();
    for (offset, raw) in text.lines().enumerate() {
        let line = offset + 1;
        let content = raw.trim();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        if raw.starts_with(char::is_whitespace) {
            let Some((_, rule)) = rules.last_mut() else {
                return Err(NamesError::at(
                    line,
                    "an indented `input` line belongs to a binding, and none is above it",
                ));
            };
            let input = parse_input(content).map_err(|e| NamesError::at(line, e))?;
            if rule.inputs.iter().any(|i| i.name == input.name) {
                return Err(NamesError::at(
                    line,
                    format!("`{}` declares the input `{}` twice", rule.from, input.name),
                ));
            }
            *rule = rule.clone().input(input);
            check(rule).map_err(|e| NamesError::at(line, e))?;
            continue;
        }
        let rule = parse_binding(content).map_err(|e| NamesError::at(line, e))?;
        if let Some((first, _)) = rules.iter().find(|(_, r)| r.from == rule.from) {
            return Err(NamesError::at(
                line,
                format!(
                    "`{}` is bound twice (first on line {first}): only one binding of a name \
                     could ever apply",
                    rule.from
                ),
            ));
        }
        rules.push((line, rule));
    }
    let mut table = AliasTable::new();
    for (line, rule) in rules {
        table = table
            .with_rule(rule)
            .map_err(|e| NamesError::at(line, e.to_string()))?;
    }
    Ok(table_rules(table))
}

/// The rules of `table` in table order, as the topology reports them.
fn table_rules(table: AliasTable) -> Vec<TopologyRule> {
    match alias_over_nothing(table).topology().kind {
        SpaceKind::Alias { rules, .. } => rules,
        _ => unreachable!("an Alias reports an Alias node"),
    }
}

/// An alias holding `table` over an empty endpoint space: what a names atom builds.
fn alias_over_nothing(table: AliasTable) -> Alias {
    Alias::new(Arc::new(table), Arc::new(EndpointSpace::new()))
}

/// Order `rules` as a table would, checking each as core does.
pub(crate) fn in_table_order(rules: Vec<TopologyRule>) -> Result<Vec<TopologyRule>, String> {
    let mut table = AliasTable::new();
    for rule in rules {
        table = table.with_rule(rule).map_err(|e| e.to_string())?;
    }
    Ok(table_rules(table))
}

/// Print bindings as a names file, one binding per line (and its declared inputs indented
/// under it), in table order. Refused for a rule the text form cannot hold: one that is not a
/// named request resolving from the root (a plain rewrite, a prefix rule), or one whose parts
/// a line cannot carry (a line break in a value, an argument name that is not one).
pub fn render_names(rules: &[TopologyRule]) -> Result<String, String> {
    let rules = in_table_order(rules.to_vec())?;
    let mut out = String::new();
    for rule in &rules {
        out.push_str(&render_binding(rule)?);
    }
    Ok(out)
}

/// One binding as its line, with its declared inputs as indented lines under it.
pub fn render_binding(rule: &TopologyRule) -> Result<String, String> {
    if rule.at != ResolveAt::Root || rule.kind != RuleKind::Exact {
        return Err(format!(
            "`{}` is a rewrite, not a named request: a names file binds whole names, each \
             resolving from the root",
            rule.from
        ));
    }
    if rule.template != rule.from.contains('{') {
        return Err(format!(
            "`{}`: a names line reads a name as a template exactly when it holds `{{`",
            rule.from
        ));
    }
    let verb = match rule.verb {
        None => "forward",
        Some(Verb::Source) => "source",
        Some(Verb::Sink) => "sink",
        Some(Verb::Exists) => "exists",
        Some(Verb::Delete) => "delete",
        Some(other) => return Err(format!("a named request cannot forward {other:?}")),
    };
    let mut line = format!(
        "{} = {verb} {}",
        quote_name(&rule.from)?,
        quote_name(&rule.to)?
    );
    for fixed in &rule.fixes {
        if !looks_like_argument_name(&fixed.name) {
            return Err(format!(
                "`{}` is not an argument name a line can write",
                fixed.name
            ));
        }
        let value = match &fixed.value {
            FixedValue::Value(value) => quote_value(value)?,
            FixedValue::Reference(iri) => format!("<{}>", iri.as_str()),
            FixedValue::Capture(var) => format!("{{{var}}}"),
            FixedValue::ReferenceTemplate(template) => format!("<{}>", template.source()),
            other => {
                return Err(format!(
                    "`{}` is fixed in a way a names line cannot say: {other}",
                    fixed.name
                ))
            }
        };
        line.push_str(&format!(" {}={value}", fixed.name));
    }
    line.push('\n');
    for input in &rule.inputs {
        if !looks_like_argument_name(&input.name) {
            return Err(format!(
                "`{}` is not an input name a line can write",
                input.name
            ));
        }
        line.push_str(&format!("  input {}", input.name));
        if !input.summary.is_empty() {
            line.push_str(&format!(" summary={}", quote_value(&input.summary)?));
        }
        if let Some(class) = &input.class {
            line.push_str(&format!(" class=<{class}>"));
        }
        if let Some(default) = &input.default {
            line.push_str(&format!(" default={}", quote_value(default)?));
        }
        for value in &input.one_of {
            line.push_str(&format!(" one-of={}", quote_value(value)?));
        }
        if input.required != input.default.is_none() {
            line.push_str(&format!(" required={}", input.required));
        }
        line.push('\n');
    }
    Ok(line)
}

/// A name or target as a word: quoted only when the tokenizer would otherwise split or
/// unquote it.
fn quote_name(text: &str) -> Result<String, String> {
    let plain = !text.is_empty()
        && text != "="
        && text != ".."
        && !text.starts_with('#')
        && !text
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\\' | '|' | '(' | ')' | ';'));
    if plain {
        Ok(text.to_string())
    } else {
        quoted(text)
    }
}

/// A value as a word: quoted as a name is, and also when it would read as a reference
/// (`<…>`) or a capture (`{…}`).
fn quote_value(text: &str) -> Result<String, String> {
    if text.starts_with('<') || text.contains('{') || text.contains('}') {
        return quoted(text);
    }
    quote_name(text)
}

fn quoted(text: &str) -> Result<String, String> {
    if text.contains(['\n', '\r']) {
        return Err(format!(
            "`{}` holds a line break, which a line-oriented names file cannot write",
            text.escape_debug()
        ));
    }
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[cfg(feature = "plan-reader")]
pub use rdf::{names_to_turtle, space, turtle_to_names, FROM_RDF, SPACE_ID, TO_RDF};

/// The Turtle face: needs core's Turtle reader (`ikigai-core/declare`), so it is the
/// `plan-reader` feature's.
#[cfg(feature = "plan-reader")]
mod rdf {
    use super::*;

    use ikigai_core::{
        space_iri, ArgRef, AsyncFnEndpoint, Description, Error, Exact, Invocation, ReprType,
        Representation, Topology, DEFAULT_MAX_HOPS,
    };

    /// `urn:names:to-rdf`: a names file → the `ik:Alias` Turtle core builds.
    pub const TO_RDF: &str = "urn:names:to-rdf";
    /// `urn:names:from-rdf`: the `ik:Alias` Turtle → its names file.
    pub const FROM_RDF: &str = "urn:names:from-rdf";
    /// The name [`space`] claims: `urn:iki:space:engine:names`.
    pub const SPACE_ID: &str = "urn:iki:space:engine:names";

    const TURTLE: &str = "text/turtle";

    /// **A names file as Turtle**: read it ([`parse_names`]), render the alias it builds with
    /// core's [`Topology::to_turtle`], and read the Turtle back with
    /// [`Topology::from_turtle`] — refusing to answer unless it reads back as the same
    /// bindings.
    pub fn names_to_turtle(text: &str) -> Result<String, NamesError> {
        let rules = parse_names(text)?;
        let mut table = AliasTable::new();
        for rule in rules {
            table = table
                .with_rule(rule)
                .map_err(|e| NamesError::at(0, e.to_string()))?;
        }
        let tree = alias_over_nothing(table).topology();
        let turtle = tree.to_turtle();
        match Topology::from_turtle(&turtle) {
            Ok(back) if back == tree => Ok(turtle),
            Ok(_) => Err(NamesError::at(
                0,
                "the bindings do not survive their own Turtle: core reads the rendering back \
                 as different bindings — refused rather than transrepted lossily",
            )),
            Err(e) => Err(NamesError::at(
                0,
                format!("the bindings do not survive their own Turtle: core refuses it: {e}"),
            )),
        }
    }

    /// **Turtle as a names file**: read an `ik:Alias` over an empty `ik:EndpointSpace` with
    /// core's [`Topology::from_turtle`], print its rules ([`render_names`]), and read the
    /// text back — refusing to answer unless it reads back as the same bindings. What a names
    /// file cannot say (a claimed space IRI, a hop budget, doors, a plain rewrite) is
    /// refused, never dropped.
    pub fn turtle_to_names(turtle: &str) -> Result<String, NamesError> {
        let tree = Topology::from_turtle(turtle).map_err(|e| NamesError::at(0, e.to_string()))?;
        let refuse = |what: &str| {
            NamesError::at(
                0,
                format!(
                    "a names file is an anonymous ik:Alias over an empty ik:EndpointSpace, and \
                     this declaration {what}: a names file cannot say it"
                ),
            )
        };
        let SpaceKind::Alias { rules, max_hops } = &tree.kind else {
            return Err(refuse("is not an alias"));
        };
        if tree.id.is_some() {
            return Err(refuse("claims a space IRI"));
        }
        if *max_hops != DEFAULT_MAX_HOPS {
            return Err(refuse("sets a hop budget"));
        }
        match &tree.children[..] {
            [leaf]
                if leaf.id.is_none()
                    && leaf.children.is_empty()
                    && matches!(&leaf.kind, SpaceKind::EndpointSpace { doors } if doors.is_empty()) =>
                {}
            _ => return Err(refuse("encloses a space with doors, a name or children")),
        }
        let text = render_names(rules).map_err(|e| NamesError::at(0, e))?;
        let ordered = in_table_order(rules.clone()).map_err(|e| NamesError::at(0, e))?;
        if parse_names(&text)? != ordered {
            return Err(NamesError::at(
                0,
                "the bindings do not survive their own names file: the printed text reads back \
                 as different bindings — refused rather than transrepted lossily",
            ));
        }
        Ok(text)
    }

    /// The two transreptors, named [`SPACE_ID`]. Configuration-free, so every call holds the
    /// same two doors and the name is a true claim.
    pub fn space() -> EndpointSpace {
        EndpointSpace::new()
            .bind(Exact::new(TO_RDF), to_rdf_endpoint())
            .bind(Exact::new(FROM_RDF), from_rdf_endpoint())
            .named(space_iri("engine:names"))
    }

    /// The text a transreptor reads: `content` (what a pipe and transreptor selection hand
    /// it), or `in`; inline, or the IRI of a resource holding it.
    async fn input(inv: &Invocation<'_>) -> Result<(String, &'static str), Error> {
        for name in ["content", "in"] {
            let bytes = match inv.request.args.get(name) {
                None => continue,
                Some(ArgRef::Inline(bytes)) => bytes.clone(),
                Some(ArgRef::Reference(iri)) => inv.source(iri).await?.bytes,
                Some(_) => {
                    return Err(Error::InvalidArgument {
                        name: name.to_string(),
                        detail: "pass the text inline or as a resource IRI".to_string(),
                    })
                }
            };
            let text = String::from_utf8(bytes).map_err(|_| Error::InvalidArgument {
                name: name.to_string(),
                detail: "not UTF-8 text".to_string(),
            })?;
            return Ok((text, name));
        }
        Err(Error::MissingArgument("content".to_string()))
    }

    async fn convert(
        inv: &Invocation<'_>,
        iri: &str,
        to: &str,
        f: fn(&str) -> Result<String, NamesError>,
    ) -> Result<Representation, Error> {
        let (text, name) = input(inv).await?;
        let out = f(&text).map_err(|e| Error::InvalidArgument {
            name: name.to_string(),
            detail: format!("{iri}: {e}"),
        })?;
        Ok(Representation::new(
            ReprType::new(to).with_param("charset", "utf-8"),
            out.into_bytes(),
        )
        .cacheable())
    }

    fn text_input(name: &str, what: &str) -> ArgSpec {
        let input = ArgSpec::new(name)
            .summary(what.to_string())
            .class("http://www.w3.org/2001/XMLSchema#string");
        if name == "in" {
            input.optional()
        } else {
            input
        }
    }

    fn to_rdf_endpoint() -> AsyncFnEndpoint {
        AsyncFnEndpoint::new("names-to-rdf", |inv| {
            Box::pin(convert(inv, TO_RDF, TURTLE, names_to_turtle))
        })
        .with_description(
            Description::new("names-to-rdf")
                .title("Names file to Turtle")
                .summary(
                    "Transrept a host's names file (one binding per line: `<name> = <verb> \
                     <target> [key=value …]`) to the ik:Alias Turtle core builds — LOSSLESSLY, \
                     checked by reading the Turtle back. Comments are not kept.",
                )
                .verb(Verb::Source)
                .verb(Verb::Meta)
                .input(text_input(
                    "content",
                    "the names file TEXT; piped in, it is this",
                ))
                .input(text_input("in", "the names file TEXT, when not piped"))
                .output(TURTLE)
                // Lossless (the default for a transreptor), and checked.
                .transreptor([MEDIA_NAMES], [TURTLE]),
        )
    }

    fn from_rdf_endpoint() -> AsyncFnEndpoint {
        AsyncFnEndpoint::new("names-from-rdf", |inv| {
            Box::pin(convert(inv, FROM_RDF, MEDIA_NAMES, turtle_to_names))
        })
        .with_description(
            Description::new("names-from-rdf")
                .title("Turtle to names file")
                .summary(
                    "Print an ik:Alias declaration of named requests (over an empty \
                     ik:EndpointSpace) as a host's names file — the inverse of \
                     urn:names:to-rdf, LOSSLESS and checked by reading the text back. What a \
                     names file cannot say is refused, never dropped.",
                )
                .verb(Verb::Source)
                .verb(Verb::Meta)
                .input(text_input(
                    "content",
                    "the Turtle TEXT; piped in, it is this",
                ))
                .input(text_input("in", "the Turtle TEXT, when not piped"))
                .output(MEDIA_NAMES)
                .transreptor([TURTLE], [MEDIA_NAMES]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikigai_core::Iri;

    /// The item's round trip: a template, a fixed reference and a plan binding (its copied
    /// inputs included), read and printed back byte for byte.
    pub(crate) const NAMES: &str = "\
urn:quakes:event:{id} = source urn:iki:fn:compose src=<urn:quakes:event.tpl> id={id}
urn:quakes:report = source urn:plan:eval in=<urn:plans:report.ttl>
  input region summary=\"the region to report on\" default=world
  input since class=<http://www.w3.org/2001/XMLSchema#date>
urn:quakes:count = source urn:sparql:select graph=urn:quakes:graph query=<urn:quakes:count.rq>
urn:quakes:page:{n} = forward urn:quakes:pages:{n} title=\"a | b\"
";

    #[test]
    fn a_names_file_round_trips_through_its_text() {
        let rules = parse_names(NAMES).unwrap();
        assert_eq!(rules.len(), 4);
        let text = render_names(&rules).unwrap();
        assert_eq!(parse_names(&text).unwrap(), rules, "{text}");
        // Printed in table order, and canonical: printing it again changes nothing.
        assert_eq!(render_names(&parse_names(&text).unwrap()).unwrap(), text);

        let event = rules
            .iter()
            .find(|r| r.from == "urn:quakes:event:{id}")
            .unwrap();
        assert!(event.template);
        assert_eq!(event.verb, Some(Verb::Source));
        assert_eq!(event.fixes[0], FixedArgument::capture("id", "id"));
        assert_eq!(
            event.fixes[1],
            FixedArgument::reference("src", Iri::parse("urn:quakes:event.tpl").unwrap())
        );
        let report = rules
            .iter()
            .find(|r| r.from == "urn:quakes:report")
            .unwrap();
        assert_eq!(report.inputs.len(), 2);
        assert_eq!(report.inputs[0].name, "region");
        assert!(!report.inputs[0].required);
        assert!(report.inputs[1].required);
        let page = rules
            .iter()
            .find(|r| r.from == "urn:quakes:page:{n}")
            .unwrap();
        assert_eq!(page.verb, None);
        assert_eq!(page.fixes[0], FixedArgument::value("title", "a | b"));
    }

    #[test]
    fn a_reference_can_contain_a_template_capture() {
        let line = "urn:river:search:{file} = source urn:sparql:select graph=urn:file:river.nt query=<urn:file:searches/{file}>";
        let rule = parse_binding(line).expect("a reference template is a valid fixed argument");
        assert!(rule.fixes.iter().any(|fixed| {
            fixed.name == "query"
                && matches!(&fixed.value, FixedValue::ReferenceTemplate(template)
                    if template.source() == "urn:file:searches/{file}")
        }));
        let rendered = render_binding(&rule).unwrap();
        assert!(rendered.contains("query=<urn:file:searches/{file}>"));
        assert_eq!(parse_binding(rendered.trim()).unwrap(), rule);
    }

    #[test]
    fn reference_templates_read_saved_searches_without_splicing_paths() {
        use futures::executor::block_on;
        use ikigai_core::{
            ArgRef, AsyncFnEndpoint, Capability, Description, Exact, FnEndpoint, Iri, Kernel,
            ReprType, Representation, Request,
        };

        let rule = parse_binding("urn:river:search:{file} = source urn:sparql:select graph=urn:file:river.nt query=<urn:file:searches/{file}>").unwrap();
        let table = AliasTable::new().with_rule(rule).unwrap();
        let select = AsyncFnEndpoint::new("select", |inv| {
            Box::pin(async move {
                let Some(ArgRef::Reference(query)) = inv.request.args.get("query") else {
                    panic!("the expanded query must be a reference");
                };
                inv.source(query).await
            })
        })
        .with_description(
            Description::new("select")
                .verb(Verb::Source)
                .input(ArgSpec::new("graph"))
                .input(ArgSpec::new("query").reads_body()),
        );
        let mut space = EndpointSpace::new().bind(Exact::new("urn:sparql:select"), select);
        for encoded in ["since.rq", "a%2Fb.rq", "a%3Ab.rq"] {
            let iri = format!("urn:file:searches/{encoded}");
            let body = iri.clone();
            space = space.bind(
                Exact::new(iri),
                FnEndpoint::new(encoded, move |_| {
                    Ok(Representation::new(
                        ReprType::new("text/plain"),
                        body.as_bytes().to_vec(),
                    ))
                }),
            );
        }
        let kernel = Kernel::new(Arc::new(space)).with_aliases(Arc::new(table));
        for (file, encoded) in [
            ("since.rq", "since.rq"),
            ("a/b.rq", "a%2Fb.rq"),
            ("a:b.rq", "a%3Ab.rq"),
        ] {
            let request = Request::new(
                Verb::Source,
                Iri::parse(format!("urn:river:search:{file}")).unwrap(),
            );
            let answer = block_on(kernel.issue(request, &Capability::root())).unwrap();
            assert_eq!(
                answer.bytes,
                format!("urn:file:searches/{encoded}").as_bytes()
            );
        }
    }

    #[test]
    fn reference_templates_validate_variables_and_preserve_quoted_text() {
        for value in [
            "<urn:file:{missing}>",
            "<urn:file:{file>",
            "<relative/{file}>",
        ] {
            assert!(
                parse_binding(&format!("urn:search:{{file}} = source urn:t query={value}"))
                    .is_err()
            );
        }
        assert!(parse_binding("urn:search = source urn:t query=<urn:file:{file}>").is_err());
        let rule =
            parse_binding("urn:search:{file} = source urn:t query=\"<urn:file:{file}>\"").unwrap();
        assert_eq!(
            rule.fixes[0],
            FixedArgument::value("query", "<urn:file:{file}>")
        );
        assert_eq!(
            parse_binding(render_binding(&rule).unwrap().trim()).unwrap(),
            rule
        );
    }

    #[cfg(feature = "plan-reader")]
    #[test]
    fn reference_templates_round_trip_through_turtle() {
        let names =
            "urn:river:search:{file} = source urn:sparql:select query=<urn:file:searches/{file}>";
        let turtle = names_to_turtle(names).unwrap();
        let text = turtle_to_names(&turtle).unwrap();
        assert_eq!(parse_names(&text).unwrap(), parse_names(names).unwrap());
        assert_eq!(names_to_turtle(&text).unwrap(), turtle);
    }

    #[test]
    fn a_capture_is_a_whole_value_never_a_splice() {
        let refused = parse_binding("urn:x:{id} = source urn:t q=a{id}").unwrap_err();
        assert!(refused.contains("WHOLE value"), "{refused}");
        // Quoted, the braces are text.
        let rule = parse_binding("urn:x:{id} = source urn:t q=\"a{id}\"").unwrap();
        assert_eq!(rule.fixes[0], FixedArgument::value("q", "a{id}"));
        // A capture of a variable the name does not have is core's refusal.
        assert!(parse_binding("urn:x:{id} = source urn:t q={nope}").is_err());
    }

    #[test]
    fn what_a_binding_cannot_mean_is_refused() {
        for (line, says) in [
            ("urn:x source urn:t", "a binding is"),
            ("urn:x = fetch urn:t", "not a verb"),
            ("urn:x = source urn:t hello", "not `key=value`"),
            ("urn:x = source urn:t a=1 a=2", "fixed twice"),
            ("urn:x = source urn:t | urn:u", "ONE request"),
            ("urn:x = source urn:t as=text/csv", "as"),
        ] {
            let refused = parse_binding(line).unwrap_err();
            assert!(refused.contains(says), "`{line}`: {refused}");
        }
        let twice = parse_names("urn:x = source urn:t\nurn:x = source urn:u\n").unwrap_err();
        assert_eq!(twice.line, 2);
        assert!(twice.detail.contains("bound twice"), "{twice}");
        let orphan = parse_names("  input a\n").unwrap_err();
        assert_eq!(orphan.line, 1);
    }

    #[test]
    fn comments_and_blank_lines_are_not_bindings() {
        let rules = parse_names("# a comment\n\nurn:x = source urn:t\n").unwrap();
        assert_eq!(rules.len(), 1);
    }

    #[cfg(feature = "plan-reader")]
    #[test]
    fn the_transreptor_pair_is_lossless_both_ways() {
        let turtle = names_to_turtle(NAMES).unwrap();
        assert!(turtle.contains("ik:Alias"), "{turtle}");
        let text = turtle_to_names(&turtle).unwrap();
        assert_eq!(parse_names(&text).unwrap(), parse_names(NAMES).unwrap());
        assert_eq!(text, render_names(&parse_names(NAMES).unwrap()).unwrap());
        // And back again, to the same Turtle.
        assert_eq!(names_to_turtle(&text).unwrap(), turtle);
    }

    #[cfg(feature = "plan-reader")]
    #[test]
    fn turtle_a_names_file_cannot_say_is_refused() {
        let named = Alias::new(
            Arc::new(AliasTable::new().exact("urn:a", "urn:b")),
            Arc::new(EndpointSpace::new()),
        )
        .topology()
        .to_turtle();
        let refused = turtle_to_names(&named).unwrap_err();
        assert!(refused.detail.contains("rewrite"), "{refused}");
    }

    #[cfg(feature = "plan-reader")]
    #[test]
    fn both_transreptors_declare_themselves_lossless() {
        use ikigai_core::select_transreptor;
        let space = space();
        let to = select_transreptor(&space, MEDIA_NAMES, "text/turtle").unwrap();
        assert_eq!(to[0].endpoint, TO_RDF);
        assert!(to[0].lossless);
        let from = select_transreptor(&space, "text/turtle", MEDIA_NAMES).unwrap();
        assert_eq!(from[0].endpoint, FROM_RDF);
        assert!(from[0].lossless);
    }
}
