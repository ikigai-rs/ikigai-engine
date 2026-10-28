//! A stored plan's `ik:pipeFrom` edge hands the next step the previous step's BODY,
//! attached as the argument the pipe fills (ledger #2490), as the text face's `|` does
//! (`tests/pipe_attach.rs`, ledger #2286 part 2): a step that declares that argument
//! `reads_body` reads the upstream as it comes, so a one-shot producer is polled only as
//! far as the step reads. An upstream with more than one consumer (a fork's branches, a
//! `..` map's items, a bound name) is collected once, as the text face collects it.
//!
//! Reader-only: without `plan-reader` a stored plan cannot be executed.
#![cfg(feature = "plan-reader")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::executor::block_on;
use futures::StreamExt;
use ikigai_core::{
    ArgRef, ArgSpec, AsyncFnEndpoint, Body, Bytes, Capability, Description, Endpoint,
    EndpointSpace, Exact, FnEndpoint, Invocation, Iri, Kernel, MetaRenderer, ReprType,
    Representation, Request, Result, Verb,
};
use ikigai_engine::engine::{Action, Engine};

/// How many lines the counting producer would yield if read to its end.
const LINES: usize = 1_000;

/// Meta rendered as the JSON description the engine routes by.
struct JsonRenderer;
impl MetaRenderer for JsonRenderer {
    fn render(&self, description: &Description, _target: &ReprType) -> Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(description).expect("serialize description"),
        ))
    }
}

/// A one-shot producer of `LINES` lines from `from`, one chunk each, counting every chunk
/// polled from it.
struct Counting {
    polled: Arc<AtomicUsize>,
}

#[async_trait]
impl Endpoint for Counting {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let body = self.invoke_body(inv).await?;
        let budget = inv.memory_budget().expect("the kernel mints one");
        body.collect_within(budget).await
    }

    async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
        let from: usize = inv
            .inline_str("from")
            .ok()
            .and_then(|from| from.parse().ok())
            .unwrap_or(0);
        let polled = self.polled.clone();
        let chunks = futures::stream::iter(from..from + LINES).map(move |n| {
            polled.fetch_add(1, Ordering::SeqCst);
            Ok(Bytes::from(format!("line {n}\n")))
        });
        Ok(Representation::from_body(
            ReprType::new("text/plain"),
            Body::once(chunks),
        ))
    }

    fn name(&self) -> &str {
        "counting"
    }

    fn describe(&self) -> Description {
        Description::new("counting").input(ArgSpec::new("from").optional())
    }
}

/// The first two lines of `text` (each with its newline).
fn first_two(text: &[u8]) -> Vec<u8> {
    text.split_inclusive(|b| *b == b'\n')
        .take(2)
        .flatten()
        .copied()
        .collect()
}

/// Read `body` until two lines are held, then drop it.
async fn two_lines(body: Body) -> Result<Vec<u8>> {
    let mut stream = body.into_stream();
    let mut held = Vec::new();
    while held.iter().filter(|b| **b == b'\n').count() < 2 {
        match stream.next().await {
            Some(chunk) => held.extend_from_slice(&chunk?),
            None => break,
        }
    }
    Ok(first_two(&held))
}

/// `urn:test:head`: the first two lines of `content`, which it declares `reads_body`.
fn head() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("head", |inv| {
        Box::pin(async move {
            let lines = two_lines(inv.content_body("content").await?).await?;
            Ok(Representation::new(ReprType::new("text/plain"), lines))
        })
    })
    .with_description(
        Description::new("head")
            .verb(Verb::Source)
            .input(ArgSpec::new("content").reads_body()),
    )
}

/// `urn:test:head-bytes`: the same answer from inline bytes, declaring nothing about bodies.
fn head_bytes() -> FnEndpoint {
    FnEndpoint::new("head-bytes", |inv| {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            first_two(inv.inline_arg("content")?),
        ))
    })
    .with_description(Description::new("head-bytes").input(ArgSpec::new("content")))
}

/// `urn:test:relay`: answers its `content` body collected, so a three-step plan has a
/// middle step that is fed a body and feeds one on.
fn relay() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("relay", |inv| {
        Box::pin(async move {
            let body = inv.content_body("content").await?;
            let bytes = inv.collect(body).await?.to_vec();
            Ok(Representation::new(ReprType::new("text/plain"), bytes))
        })
    })
    .with_description(
        Description::new("relay")
            .verb(Verb::Source)
            .input(ArgSpec::new("content").reads_body()),
    )
}

/// `urn:test:store`: a SINK that keeps the first two lines of `content`, read as a body.
fn store(kept: Arc<Mutex<Vec<u8>>>) -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("store", move |inv| {
        let kept = kept.clone();
        Box::pin(async move {
            let lines = two_lines(inv.content_body("content").await?).await?;
            let answer = format!("stored {} bytes", lines.len());
            *kept.lock().unwrap() = lines;
            Ok(Representation::new(
                ReprType::new("text/plain"),
                answer.into_bytes(),
            ))
        })
    })
    .with_description(
        Description::new("store")
            .verb(Verb::Sink)
            .input(ArgSpec::new("content").reads_body()),
    )
}

struct World {
    engine: Engine,
    kernel: Arc<Kernel>,
    polled: Arc<AtomicUsize>,
    plan: Arc<Mutex<String>>,
    kept: Arc<Mutex<Vec<u8>>>,
}

fn world() -> World {
    let polled = Arc::new(AtomicUsize::new(0));
    let plan: Arc<Mutex<String>> = Arc::default();
    let kept: Arc<Mutex<Vec<u8>>> = Arc::default();
    let held = plan.clone();
    let space = EndpointSpace::new()
        .bind(
            Exact::new("urn:t:plan"),
            FnEndpoint::new("plan-store", move |_: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/turtle"),
                    held.lock().unwrap().clone().into_bytes(),
                ))
            })
            .with_description(Description::new("plan-store").verb(Verb::Source)),
        )
        .bind(
            Exact::new("urn:test:count"),
            Counting {
                polled: polled.clone(),
            },
        )
        .bind(Exact::new("urn:test:head"), head())
        .bind(Exact::new("urn:test:head-bytes"), head_bytes())
        .bind(Exact::new("urn:test:relay"), relay())
        .bind(Exact::new("urn:test:store"), store(kept.clone()));
    let kernel = Arc::new(Kernel::with_meta_renderer(
        Arc::new(ikigai_core::Fallback::new(vec![
            Arc::new(space) as Arc<dyn ikigai_core::Space>,
            Arc::new(ikigai_shacl::space()),
            Arc::new(ikigai_engine::plan_space::space()),
        ])),
        Arc::new(JsonRenderer),
    ));
    World {
        engine: Engine::new(kernel.clone()),
        kernel,
        polled,
        plan,
        kept,
    }
}

fn output(engine: &Engine, line: &str) -> std::result::Result<String, String> {
    match engine.eval(line) {
        Action::Output(entry) => entry.result,
        _ => panic!("`{line}` should produce output"),
    }
}

impl World {
    /// Render `spec` as a plan, store it, and `execute` it: the counter is reset after
    /// rendering, so it counts only what the run polled.
    fn execute(&self, spec: &str) -> std::result::Result<String, String> {
        let turtle = output(&self.engine, &format!("plan {spec}")).expect("the spec renders");
        *self.plan.lock().unwrap() = turtle;
        self.polled.store(0, Ordering::SeqCst);
        output(&self.engine, "execute urn:t:plan")
    }

    /// The same stored plan through `urn:plan:eval`, as a sub-request would issue it.
    fn eval(&self) -> ikigai_core::Result<Representation> {
        let turtle = self.plan.lock().unwrap().clone();
        let request = Request::new(Verb::Source, Iri::parse("urn:plan:eval").unwrap())
            .with_arg("in", ArgRef::Inline(turtle.into_bytes()));
        block_on(self.kernel.issue(request, &Capability::root()))
    }

    fn polled(&self) -> usize {
        self.polled.load(Ordering::SeqCst)
    }
}

#[test]
fn a_piped_step_that_reads_its_body_polls_the_upstream_only_as_far_as_it_reads() {
    for spec in [
        "urn:test:count | urn:test:head",
        "urn:test:count from=0 | urn:test:head",
    ] {
        let world = world();
        assert_eq!(
            world.execute(spec).as_deref(),
            Ok("line 0\nline 1\n"),
            "{spec}"
        );
        let polled = world.polled();
        assert!(
            polled <= 2,
            "`execute` of `{spec}`: the producer was polled {polled} times for a step that \
             read two lines (it yields {LINES}): the plan runner collected the upstream \
             instead of attaching its body"
        );
    }
}

#[test]
fn a_body_streams_through_a_middle_step_and_into_a_sink() {
    let world = world();
    assert_eq!(
        world
            .execute("urn:test:count from=5 | urn:test:relay | urn:test:head")
            .as_deref(),
        Ok("line 5\nline 6\n")
    );
    assert_eq!(
        world
            .execute("urn:test:count from=3 | sink urn:test:store")
            .as_deref(),
        Ok("stored 14 bytes")
    );
    assert_eq!(world.kept.lock().unwrap().as_slice(), b"line 3\nline 4\n");
    let polled = world.polled();
    assert!(polled <= 2, "the sink's upstream was polled {polled} times");
}

#[test]
fn an_undeclared_step_is_handed_the_same_bytes() {
    let world = world();
    let by_value = world.execute("urn:test:count from=0 | urn:test:head-bytes");
    assert_eq!(by_value.as_deref(), Ok("line 0\nline 1\n"));
    assert_eq!(world.polled(), LINES, "collected whole");
}

#[test]
fn an_upstream_with_several_consumers_is_collected_once() {
    let world = world();
    assert_eq!(
        world
            .execute(
                "urn:test:count from=0 | ( urn:test:head ; urn:test:head-bytes ; urn:test:head )"
            )
            .as_deref(),
        Ok("line 0\nline 1\n\nline 0\nline 1\n\nline 0\nline 1\n")
    );
    assert_eq!(world.polled(), LINES, "collected once, for every branch");
    assert_eq!(
        world
            .execute("urn:test:count from=0 | urn:test:head .. urn:test:head")
            .as_deref(),
        Ok("line 0\nline 1\n")
    );
}

#[test]
fn urn_plan_eval_answers_what_execute_answers() {
    let world = world();
    for spec in [
        "urn:test:count from=0 | urn:test:head",
        "urn:test:count from=5 | urn:test:relay | urn:test:head",
        "urn:test:count from=0 | ( urn:test:head ; urn:test:head-bytes )",
    ] {
        let executed = world.execute(spec).expect("execute");
        let evaluated = world.eval().expect("urn:plan:eval");
        assert_eq!(evaluated.bytes, executed.as_bytes(), "{spec}");
    }
}

#[test]
fn an_upstream_a_later_step_also_references_by_name_is_collected_once() {
    // Step 1 is read twice: piped into step 2, and referenced (`@counted`) by step 3. A
    // body is read once, so it is collected, and both readers are handed the same bytes.
    let world = world();
    *world.plan.lock().unwrap() = r#"
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:plan:twice> a ik:Process ;
    ik:step <urn:plan:twice:step:1> , <urn:plan:twice:step:2> , <urn:plan:twice:step:3> ;
    ik:result <urn:plan:twice:step:3> .
<urn:plan:twice:step:1> a ik:Step ;
    ik:verb "Source" ;
    ik:resolves <urn:test:count> ;
    ik:binds "counted" .
<urn:plan:twice:step:2> a ik:Step ;
    ik:verb "Source" ;
    ik:resolves <urn:test:head> ;
    ik:pipeFrom <urn:plan:twice:step:1> .
<urn:plan:twice:step:3> a ik:Step ;
    ik:verb "Source" ;
    ik:resolves <urn:test:head-bytes> ;
    ik:pipeFrom <urn:plan:twice:step:2> ;
    ik:argument <urn:plan:twice:step:3:arg:seen> .
<urn:plan:twice:step:3:arg:seen> a ik:Argument ;
    ik:inputName "seen" ;
    ik:ref <urn:plan:twice:var:counted> .
"#
    .to_string();
    world.polled.store(0, Ordering::SeqCst);
    assert_eq!(
        output(&world.engine, "execute urn:t:plan").as_deref(),
        Ok("line 0\nline 1\n")
    );
    assert_eq!(world.polled(), LINES, "collected once, for both readers");
}
