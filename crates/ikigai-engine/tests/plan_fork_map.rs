//! A mapped fork in a plan (`a .. ( b ; c )`, `ik:mapOver` on an `ik:Fork`, ledger #2654):
//! each item is its own request, with its own cache entry and thread.
//!
//! The evidence is the text face's own tally, `[n cached · m computed]`: of four items,
//! change one, and the next run of the stored plan recomputes exactly that item's branches.
//!
//! Reader-only: without `plan-reader` there is no `execute`.
#![cfg(feature = "plan-reader")]

use std::sync::{Arc, Mutex};

use ikigai_core::{
    ArgSpec, Description, EndpointSpace, Exact, FnEndpoint, Invocation, Kernel, MetaRenderer,
    ReprType, Representation, Verb,
};
use ikigai_engine::{Action, Engine, Entry};

/// The JSON Meta face the engine reads a contract through, so a fed item routes to `in`
/// because the contract says so.
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

/// A cacheable branch: `{tag}({in})`, recording each request it actually computes.
fn branch(tag: &'static str, log: &Log) -> FnEndpoint {
    let log = Arc::clone(log);
    FnEndpoint::new(tag, move |inv: &Invocation<'_>| {
        let input = inv.inline_str("in")?.to_string();
        log.lock().expect("log").push(format!("{tag}({input})"));
        Ok(Representation::new(
            ReprType::new("text/plain"),
            format!("{tag}({input})").into_bytes(),
        )
        .cacheable())
    })
    .with_description(
        Description::new(tag)
            .verb(Verb::Source)
            .input(ArgSpec::new("in")),
    )
}

/// The engine, the stored plan `urn:t:plan`, and what the branches computed.
///
/// `urn:t:list` is a cacheable list of four items; a `sink` to it replaces the THIRD item
/// with its content, and the kernel cuts its thread on the write, as for every write.
fn world() -> (Engine, Arc<Mutex<String>>, Log) {
    let log: Log = Arc::default();
    let items = Arc::new(Mutex::new(["a", "b", "c", "d"].join("\n")));
    let list = FnEndpoint::new("list", move |inv: &Invocation<'_>| {
        if inv.request.verb == Verb::Sink {
            let content = inv.inline_str("content")?.to_string();
            let mut held = items.lock().expect("items");
            let mut lines: Vec<String> = held.lines().map(str::to_string).collect();
            lines[2] = content;
            *held = lines.join("\n");
            return Ok(Representation::new(
                ReprType::new("text/plain"),
                b"ok".to_vec(),
            ));
        }
        Ok(Representation::new(
            ReprType::new("text/plain"),
            items.lock().expect("items").clone().into_bytes(),
        )
        .cacheable())
    })
    .with_description(
        Description::new("list")
            .verb(Verb::Source)
            .verb(Verb::Sink)
            .input(ArgSpec::new("content").optional()),
    );
    let stored: Arc<Mutex<String>> = Arc::default();
    let held = Arc::clone(&stored);
    let plan = FnEndpoint::new("plan", move |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new("text/turtle"),
            held.lock().expect("plan").clone().into_bytes(),
        )
        .cacheable())
    })
    .with_description(Description::new("plan").verb(Verb::Source));
    let space = EndpointSpace::new()
        .bind(Exact::new("urn:t:list"), list)
        .bind(Exact::new("urn:t:b"), branch("b", &log))
        .bind(Exact::new("urn:t:c"), branch("c", &log))
        .bind(Exact::new("urn:t:plan"), plan);
    let engine = Engine::new(Kernel::with_meta_renderer(
        Arc::new(space),
        Arc::new(JsonRenderer),
    ));
    (engine, stored, log)
}

fn entry(action: Action) -> Entry {
    match action {
        Action::Output(entry) => entry,
        _ => panic!("expected output"),
    }
}

fn drain(log: &Log) -> Vec<String> {
    std::mem::take(&mut *log.lock().expect("log"))
}

const SPEC: &str = "urn:t:list .. ( urn:t:b ; urn:t:c )";

#[test]
fn each_item_of_a_mapped_fork_is_its_own_request_and_cache_entry() {
    let (engine, stored, log) = world();
    *stored.lock().expect("plan") = entry(engine.eval(&format!("plan {SPEC}")))
        .result
        .expect("plan");
    assert!(stored.lock().unwrap().contains("ik:mapOver"));

    // Cold: the plan, the list, and two branches for each of four items.
    let cold = entry(engine.eval("execute urn:t:plan"));
    let answer = "b(a)\nc(a)\nb(b)\nc(b)\nb(c)\nc(c)\nb(d)\nc(d)";
    assert_eq!(cold.result.as_deref(), Ok(answer));
    assert_eq!(cold.cache.label().as_deref(), Some("10 computed"));
    assert_eq!(drain(&log).len(), 8);

    // Nothing changed: every request is a cache hit, and no branch computes.
    let warm = entry(engine.eval("execute urn:t:plan"));
    assert_eq!(warm.result.as_deref(), Ok(answer));
    assert_eq!(warm.cache.label().as_deref(), Some("10 cached"));
    assert!(drain(&log).is_empty());

    // Change the third item. The list is recomputed (its thread was cut), and of the
    // branches exactly the changed item's two: the other three items' are the same
    // requests as before, served from their own cache entries.
    entry(engine.eval("sink urn:t:list C"));
    let cut = entry(engine.eval("execute urn:t:plan"));
    assert_eq!(
        cut.result.as_deref(),
        Ok("b(a)\nc(a)\nb(b)\nc(b)\nb(C)\nc(C)\nb(d)\nc(d)")
    );
    assert_eq!(cut.cache.label().as_deref(), Some("7 cached · 3 computed"));
    assert_eq!(drain(&log), ["b(C)", "c(C)"]);

    // The text face runs the same spec to the same answer.
    let text = entry(engine.eval(&format!("source {SPEC}")));
    assert_eq!(text.result, cut.result);
}

#[test]
fn a_failing_item_aborts_the_mapped_fork() {
    // As `run_map` does in the text face: a failure as a value is a separate decision.
    let (engine, stored, log) = world();
    *stored.lock().expect("plan") =
        entry(engine.eval("plan urn:t:list .. ( urn:t:b ; urn:t:missing )"))
            .result
            .expect("plan");
    let failed = entry(engine.eval("execute urn:t:plan"));
    assert!(failed.result.is_err(), "{:?}", failed.result);
    assert_eq!(
        drain(&log),
        ["b(a)"],
        "the first item's failure stops the run"
    );
}
