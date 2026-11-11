//! A script of REPL lines (`.iki`, ledger #2485): comments, a shebang, declared parameters
//! and stop-on-error, through [`Engine::eval`] and [`Engine::run_script`].
//!
//! Not feature-gated: running a script needs no plan reader, so a wasm host has it too.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ikigai_core::{
    ArgSpec, Description, EndpointSpace, Error, Exact, FnEndpoint, Invocation, Kernel,
    MetaRenderer, ReprType, Representation, Verb,
};
use ikigai_engine::{Action, Engine, ScriptRun};

/// The engine routes a positional value by the target's contract, read as JSON Meta.
struct JsonRenderer;
impl MetaRenderer for JsonRenderer {
    fn render(
        &self,
        description: &Description,
        _target: &ReprType,
    ) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(description).expect("serialize description"),
        ))
    }
}

type Log = Arc<Mutex<Vec<String>>>;

/// `urn:text:upper` upper-cases `in`, `urn:t:fail` always fails, and `urn:t:mark` records
/// its `in`, so a test can see which lines ran. `urn:t:inner.iki` is a stored script
/// ([`INNER`]) and `urn:t:loop.iki` one that runs itself.
fn engine() -> (Engine, Log) {
    let log: Log = Arc::default();
    let marks = Arc::clone(&log);
    let text = |name: &str| {
        Description::new(name)
            .verb(Verb::Source)
            .input(ArgSpec::new("in").summary("the text"))
            .output("text/plain")
    };
    let space = EndpointSpace::new()
        .bind(
            Exact::new("urn:text:upper"),
            FnEndpoint::new("upper", |inv: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    inv.inline_str("in")?.to_uppercase().into_bytes(),
                ))
            })
            .with_description(text("upper")),
        )
        .bind(
            Exact::new("urn:t:mark"),
            FnEndpoint::new("mark", move |inv: &Invocation<'_>| {
                let value = inv.inline_str("in")?.to_string();
                marks.lock().expect("log").push(value.clone());
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    value.into_bytes(),
                ))
            })
            .with_description(text("mark")),
        )
        .bind(
            Exact::new("urn:t:inner.iki"),
            FnEndpoint::new("inner", |_: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    INNER.as_bytes().to_vec(),
                ))
            }),
        )
        .bind(
            Exact::new("urn:t:loop.iki"),
            FnEndpoint::new("loop", |_: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    b"script urn:t:loop.iki\n".to_vec(),
                ))
            }),
        )
        .bind(
            Exact::new("urn:t:fail"),
            FnEndpoint::new("fail", |_: &Invocation<'_>| {
                Err(Error::Refused("this line fails".to_string()))
            })
            .with_description(text("fail")),
        );
    let engine = Engine::new(Arc::new(Kernel::with_meta_renderer(
        Arc::new(space),
        Arc::new(JsonRenderer),
    )));
    (engine, log)
}

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Each line's printed answer, in order (`Noop` and the like print nothing).
fn answers(run: &ScriptRun) -> Vec<(usize, Result<String, String>)> {
    run.actions
        .iter()
        .filter_map(|(line, action)| match action {
            Action::Output(entry) => Some((*line, entry.result.clone())),
            _ => None,
        })
        .collect()
}

const GREET: &str = "#!/usr/bin/env ikigai\n\
                     # Greet someone, loudly.\n\
                     # @param who = world\n\
                     \n\
                     source urn:text:upper in={who}\n";

/// A stored script another runs with `script`: it marks its parameter, then upper-cases it.
const INNER: &str = "# @param who = world\n\
                     source urn:t:mark in={who}\n\
                     source urn:text:upper in={who}\n";

// --- comments ----------------------------------------------------------------------------

#[test]
fn a_comment_line_is_not_a_command() {
    let (engine, _) = engine();
    for line in [
        "# a comment line",
        "   # indented",
        "#!/usr/bin/env ikigai",
        "# @param who",
    ] {
        assert!(
            matches!(engine.eval(line), Action::Noop),
            "`{line}` should be a comment"
        );
    }
}

#[test]
fn a_hash_anywhere_else_keeps_its_meaning() {
    let (engine, _) = engine();
    match engine.eval("source urn:text:upper in=#243") {
        Action::Output(entry) => assert_eq!(entry.result, Ok("#243".to_string())),
        _ => panic!("expected output"),
    }
}

// --- parameters --------------------------------------------------------------------------

#[test]
fn a_script_with_a_shebang_a_comment_and_a_parameter_runs() {
    let (engine, _) = engine();
    let run = engine.run_script(GREET, &BTreeMap::new());
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(answers(&run), vec![(5, Ok("WORLD".to_string()))]);
}

#[test]
fn a_given_parameter_answers_through_the_line() {
    let (engine, _) = engine();
    let run = engine.run_script(GREET, &args(&[("who", "brian")]));
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(answers(&run), vec![(5, Ok("BRIAN".to_string()))]);
}

#[test]
fn a_value_that_would_split_is_still_one_word() {
    let (engine, _) = engine();
    let run = engine.run_script(GREET, &args(&[("who", "two | \"words\"")]));
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(answers(&run), vec![(5, Ok("TWO | \"WORDS\"".to_string()))]);

    let quoted = "# @param who\nsource urn:text:upper \"in=hi {who}\"\n";
    let run = engine.run_script(quoted, &args(&[("who", "a \"b\"")]));
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(answers(&run), vec![(2, Ok("HI A \"B\"".to_string()))]);
}

#[test]
fn an_undeclared_capture_is_refused_naming_its_line_before_anything_runs() {
    let (engine, log) = engine();
    let script = "# @param who\nsource urn:t:mark in={who}\nsource urn:t:mark in={x}\n";
    let run = engine.run_script(script, &args(&[("who", "brian")]));
    let error = run.error.expect("refused");
    assert_eq!(error.line, Some(3));
    assert!(
        error
            .to_string()
            .starts_with("line 3: `{x}` names no declared parameter"),
        "{error}"
    );
    assert!(run.actions.is_empty());
    assert!(log.lock().unwrap().is_empty(), "no line may run");
}

#[test]
fn a_brace_around_something_else_is_not_a_capture() {
    let (engine, _) = engine();
    let run = engine.run_script("source urn:text:upper \"in={\\\"a\\\": {}}\"\n", &args(&[]));
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(answers(&run), vec![(1, Ok("{\"A\": {}}".to_string()))]);
}

#[test]
fn an_undeclared_parameter_is_refused_by_name() {
    let (engine, _) = engine();
    let run = engine.run_script(GREET, &args(&[("nobody", "x")]));
    let error = run.error.expect("refused");
    assert_eq!(
        error.to_string(),
        "the script declares no parameter `nobody` (it declares who)"
    );
}

#[test]
fn a_parameter_without_a_default_must_be_given() {
    let (engine, _) = engine();
    let run = engine.run_script("# @param who\nsource urn:text:upper in={who}\n", &args(&[]));
    let error = run.error.expect("refused");
    assert!(
        error.to_string().contains("`who` has no default"),
        "{error}"
    );
}

#[test]
fn a_malformed_declaration_is_refused_naming_its_line() {
    let (engine, _) = engine();
    for (script, line, words) in [
        ("# ok\n# @param\n", 2, "needs a name"),
        ("# @param 1st\n", 1, "identifier"),
        ("# @param a\n# @param a = b\n", 2, "declared twice"),
    ] {
        let error = engine
            .run_script(script, &args(&[]))
            .error
            .unwrap_or_else(|| panic!("`{script}` refused"));
        assert_eq!(error.line, Some(line), "{script}");
        assert!(error.message.contains(words), "{error}");
    }
}

#[test]
fn a_quoted_default_is_the_text_inside() {
    let (engine, _) = engine();
    let script = "# @param who = \"two words\"\nsource urn:text:upper in={who}\n";
    let run = engine.run_script(script, &args(&[]));
    assert_eq!(answers(&run), vec![(2, Ok("TWO WORDS".to_string()))]);
}

// --- stop on error -----------------------------------------------------------------------

#[test]
fn a_failing_line_stops_the_run_before_the_next() {
    let (engine, log) = engine();
    let script = "source urn:t:mark in=one\n\
                  # the next line fails\n\
                  source urn:t:fail in=two\n\
                  source urn:t:mark in=three\n";
    let run = engine.run_script(script, &args(&[]));
    assert!(!run.succeeded());
    let error = run.error.as_ref().expect("stopped");
    assert_eq!(error.line, Some(3));
    assert!(error.message.contains("this line fails"), "{error}");
    assert_eq!(*log.lock().unwrap(), vec!["one".to_string()]);
    assert_eq!(answers(&run).len(), 2, "line 4 never ran");
}

#[test]
fn an_unknown_command_stops_the_run_too() {
    let (engine, log) = engine();
    let script = "source urn:t:mark in=one\nfrobnicate\nsource urn:t:mark in=three\n";
    let error = engine
        .run_script(script, &args(&[]))
        .error
        .expect("stopped");
    assert_eq!(error.line, Some(2));
    assert!(
        error.message.contains("unknown command `frobnicate`"),
        "{error}"
    );
    assert_eq!(*log.lock().unwrap(), vec!["one".to_string()]);
}

#[test]
fn quit_ends_a_script_without_error() {
    let (engine, log) = engine();
    let run = engine.run_script(
        "source urn:t:mark in=one\nquit\nsource urn:t:mark in=two\n",
        &args(&[]),
    );
    assert!(run.succeeded());
    assert_eq!(*log.lock().unwrap(), vec!["one".to_string()]);
}

#[test]
fn a_doubled_brace_is_template_text_not_a_capture() {
    // Ledger #3186: a script that writes a template (`$h{{mag}}`) carries its placeholders
    // through as they are, beside a parameter of its own on the same line.
    let (engine, _) = engine();
    let script = "# @param who = world\n\
                  source urn:t:mark \"in=$h{{mag}} to {who}, {{ who }} {{who}}\"\n\
                  source urn:t:mark in=$h{{mag}}\n";
    let run = engine.run_script(script, &args(&[]));
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(
        answers(&run),
        vec![
            (2, Ok("$h{{mag}} to world, {{ who }} {{who}}".to_string())),
            (3, Ok("$h{{mag}}".to_string())),
        ]
    );
}

#[test]
fn an_undeclared_capture_inside_quotes_is_refused_too() {
    // Before #3186 only an unquoted capture was checked, so this one panicked the engine.
    let (engine, log) = engine();
    let run = engine.run_script("source urn:t:mark \"in=hi {x}\"\n", &args(&[]));
    let error = run.error.expect("refused");
    assert_eq!(error.line, Some(1));
    assert!(
        error.message.contains("`{x}` names no declared parameter"),
        "{error}"
    );
    assert!(log.lock().unwrap().is_empty(), "no line may run");
}

// --- a script that runs another (ledger #3185) -------------------------------------------

#[test]
fn a_script_runs_a_stored_script_as_the_repl_does() {
    let (engine, log) = engine();
    let script = "source urn:t:mark in=before\n\
                  script urn:t:inner.iki who=\"two words\"\n\
                  source urn:t:mark in=after\n";
    let run = engine.run_script(script, &args(&[]));
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(
        answers(&run),
        vec![
            (1, Ok("before".to_string())),
            (2, Ok("two words\nTWO WORDS".to_string())),
            (3, Ok("after".to_string())),
        ]
    );
    assert_eq!(*log.lock().unwrap(), vec!["before", "two words", "after"]);
}

#[test]
fn a_stored_script_takes_its_defaults_and_refuses_an_undeclared_parameter() {
    let (engine, _) = engine();
    match engine.eval("script urn:t:inner.iki") {
        Action::Output(entry) => assert_eq!(entry.result, Ok("world\nWORLD".to_string())),
        _ => panic!("expected output"),
    }
    let error = engine
        .run_script("script urn:t:inner.iki nobody=x\n", &args(&[]))
        .error
        .expect("refused");
    assert_eq!(error.line, Some(1));
    assert!(
        error.message.contains("declares no parameter `nobody`"),
        "{error}"
    );
}

#[test]
fn a_failing_line_of_a_stored_script_stops_both_and_names_its_line() {
    let (engine, log) = engine();
    let script = "script urn:t:inner.iki who=x\n\
                  script urn:t:inner.iki who\n\
                  source urn:t:mark in=never\n";
    let error = engine
        .run_script(script, &args(&[]))
        .error
        .expect("stopped");
    assert_eq!(error.line, Some(2));
    assert!(
        error.message.contains("`who` is not a parameter"),
        "{error}"
    );

    let script = "source urn:t:mark in=one\nscript urn:t:missing.iki\nsource urn:t:mark in=two\n";
    let error = engine
        .run_script(script, &args(&[]))
        .error
        .expect("stopped");
    assert_eq!(error.line, Some(2));
    assert!(error.message.contains("urn:t:missing.iki"), "{error}");
    assert_eq!(*log.lock().unwrap(), vec!["x", "one"]);
}

#[test]
fn a_script_that_runs_itself_is_stopped() {
    let (engine, _) = engine();
    let error = engine
        .run_script("script urn:t:loop.iki\n", &args(&[]))
        .error
        .expect("stopped");
    assert_eq!(error.line, Some(1));
    assert_eq!(
        error.message,
        "the script urn:t:loop.iki stopped at line 1: the script urn:t:loop.iki is already \
         running, so it would run itself without end: urn:t:loop.iki → urn:t:loop.iki"
    );
    // The refusal unwound the run: the same engine runs the stored script again.
    match engine.eval("script urn:t:inner.iki") {
        Action::Output(entry) => assert_eq!(entry.result, Ok("world\nWORLD".to_string())),
        _ => panic!("expected output"),
    }
}
