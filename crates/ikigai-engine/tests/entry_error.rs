//! `Entry::error` (ledger #3080): the typed [`ikigai_core::Error`] behind a failed line, so a
//! frontend that maps errors to its own words (a player-facing door) reads the variant from
//! the entry instead of wrapping the `Resolver` to capture it.

use std::sync::Arc;

use ikigai_core::{
    ArgSpec, Description, EndpointSpace, Error, Exact, FnEndpoint, Invocation, Iri, Kernel,
    MetaRenderer, ReprType, Representation, Result, Verb,
};
use ikigai_engine::engine::{Action, Engine, Entry};

struct JsonRenderer;
impl MetaRenderer for JsonRenderer {
    fn render(&self, description: &Description, _target: &ReprType) -> Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(description).expect("serialize description"),
        ))
    }
}

/// `urn:test:echo` answers its `in`; `urn:test:taken` refuses with a typed `Conflict`.
fn engine() -> Engine {
    let echo = FnEndpoint::new("echo", |inv: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            inv.inline_str("in")?.as_bytes().to_vec(),
        ))
    })
    .with_description(
        Description::new("echo")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(ArgSpec::new("in"))
            .output("text/plain"),
    );
    let taken = FnEndpoint::new("taken", |_: &Invocation<'_>| {
        Err(Error::Conflict("that square is taken".to_string()))
    })
    .with_description(
        Description::new("taken")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .output("text/plain"),
    );
    let space = EndpointSpace::new()
        .bind(Exact::new("urn:test:echo"), echo)
        .bind(Exact::new("urn:test:taken"), taken);
    Engine::new(Kernel::with_meta_renderer(
        Arc::new(space),
        Arc::new(JsonRenderer),
    ))
}

fn entry(action: Action) -> Entry {
    match action {
        Action::Output(entry) => entry,
        _ => panic!("expected Action::Output"),
    }
}

#[test]
fn a_success_carries_no_error() {
    let entry = entry(engine().eval("source urn:test:echo hello"));
    assert_eq!(entry.result.as_deref(), Ok("hello"));
    assert_eq!(entry.error, None);
}

#[test]
fn an_endpoint_refusal_carries_its_typed_error() {
    let entry = entry(engine().eval("source urn:test:taken"));
    assert!(entry.result.is_err());
    assert_eq!(
        entry.error,
        Some(Error::Conflict("that square is taken".to_string()))
    );
}

#[test]
fn an_unbound_name_carries_unresolved() {
    let entry = entry(engine().eval("source urn:test:nowhere"));
    let message = entry.result.expect_err("an unbound name fails");
    assert_eq!(
        entry.error,
        Some(Error::Unresolved(Iri::parse("urn:test:nowhere").unwrap()))
    );
    // The message is unchanged: the typed error rides beside it.
    assert!(message.starts_with(&entry.error.unwrap().to_string()));
}

#[test]
fn a_missing_argument_carries_its_name() {
    let entry = entry(engine().eval("source urn:test:echo"));
    assert!(entry.result.is_err());
    assert_eq!(entry.error, Some(Error::MissingArgument("in".to_string())));
}

#[test]
fn a_failed_pipe_stage_carries_that_stage_error() {
    let entry = entry(engine().eval("source urn:test:echo hi | urn:test:taken"));
    assert!(entry.result.is_err());
    assert_eq!(
        entry.error,
        Some(Error::Conflict("that square is taken".to_string()))
    );
}

#[test]
fn a_failed_lisp_buffer_through_eval_lisp_carries_unresolved() {
    // No `urn:lisp:eval` is bound here, so the Lisp seam fails to resolve.
    let entry = entry(engine().eval_lisp("(+ 1 2)"));
    assert!(entry.result.is_err());
    assert_eq!(
        entry.error,
        Some(Error::Unresolved(Iri::parse("urn:lisp:eval").unwrap()))
    );
}

#[test]
fn an_engine_side_error_carries_none() {
    // The engine refuses this itself; no resolver error is behind it.
    let entry = entry(engine().eval("frobnicate"));
    assert!(entry.result.is_err());
    assert_eq!(entry.error, None);
}

#[test]
fn an_error_does_not_leak_into_the_next_line() {
    let engine = engine();
    let failed = entry(engine.eval("source urn:test:taken"));
    assert!(failed.error.is_some());
    let next = entry(engine.eval("frobnicate"));
    assert!(next.result.is_err());
    assert_eq!(next.error, None);
}
