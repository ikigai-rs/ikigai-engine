//! A pipe hands the next stage the previous stage's BODY, attached as the argument the pipe
//! fills (ledger #2286, part 2, on core 0.2.3's `BodyIssue`): a stage that declares that
//! argument `reads_body` reads the upstream as it comes, so a one-shot producer is polled
//! only as far as the stage reads. A stage that declares nothing is handed the same bytes,
//! collected by the kernel under the run's byte budget, and the trace says so.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use ikigai_core::{
    ArgSpec, AsyncFnEndpoint, Body, Bytes, Capability, Description, Endpoint, EndpointSpace, Exact,
    FnEndpoint, Invocation, Kernel, MetaRenderer, ReprType, Representation, Request, Resolution,
    Result, Scope, Space, SpaceEntry, TraceEvent, Tracer, Verb, COLLECTED_NOTE,
};
use ikigai_engine::engine::{Action, Engine};
use ikigai_resolve::{CacheStatus, RemoteSpace, Resolver};

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

/// A one-shot producer of `LINES` lines, one chunk each, counting every chunk polled
/// from it. `urn:test:count` takes no argument; `urn:test:count-from` takes `from`, so it
/// is not the bare upstream the by-reference path (part 1) reads.
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

/// `urn:test:head`: the first two lines of `content`, which it declares `reads_body` (and
/// nothing else: it is no transreptor).
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

/// `urn:test:all`: the whole of `content`, read as a body and collected under the run's
/// byte budget.
fn all() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("all", |inv| {
        Box::pin(async move {
            let body = inv.content_body("content").await?;
            Ok(Representation::new(
                ReprType::new("text/plain"),
                inv.collect(body).await?.to_vec(),
            ))
        })
    })
    .with_description(
        Description::new("all")
            .verb(Verb::Source)
            .input(ArgSpec::new("content").reads_body()),
    )
}

/// `urn:test:relay`: answers its `content` body as it came, unread, so a three-stage line
/// streams through it.
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

/// `urn:test:store`: a SINK that keeps the first two lines of `content`, read as a body,
/// in `kept`.
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

fn space(polled: &Arc<AtomicUsize>, kept: &Arc<Mutex<Vec<u8>>>) -> EndpointSpace {
    EndpointSpace::new()
        .bind(
            Exact::new("urn:test:count"),
            Counting {
                polled: polled.clone(),
            },
        )
        .bind(Exact::new("urn:test:head"), head())
        .bind(Exact::new("urn:test:head-bytes"), head_bytes())
        .bind(Exact::new("urn:test:all"), all())
        .bind(Exact::new("urn:test:relay"), relay())
        .bind(Exact::new("urn:test:store"), store(kept.clone()))
}

fn kernel(polled: &Arc<AtomicUsize>) -> Kernel {
    let kept = Arc::new(Mutex::new(Vec::new()));
    Kernel::with_meta_renderer(Arc::new(space(polled, &kept)), Arc::new(JsonRenderer))
}

fn output(engine: &Engine, line: &str) -> std::result::Result<String, String> {
    match engine.eval(line) {
        Action::Output(entry) => entry.result,
        _ => panic!("`{line}` should produce output"),
    }
}

/// Every trace note the kernel recorded, as `key=value`.
#[derive(Default)]
struct Notes(Mutex<Vec<String>>);
impl Tracer for Notes {
    fn record(&self, event: TraceEvent) {
        let mut notes = self.0.lock().unwrap();
        for (key, value) in event.notes {
            notes.push(format!("{key}={value}"));
        }
    }
}

#[test]
fn a_stage_that_reads_its_body_polls_the_upstream_only_as_far_as_it_reads() {
    // `from=` makes the upstream a request with an argument, which part 1's by-reference
    // path does not take; `urn:test:head` is no transreptor either.
    for line in [
        "source urn:test:count | urn:test:head",
        "source urn:test:count from=0 | urn:test:head",
    ] {
        let polled = Arc::new(AtomicUsize::new(0));
        let engine = Engine::new(kernel(&polled));
        assert_eq!(
            output(&engine, line).as_deref(),
            Ok("line 0\nline 1\n"),
            "{line}"
        );
        let polled = polled.load(Ordering::SeqCst);
        assert!(
            polled <= 2,
            "`{line}`: the producer was polled {polled} times for a stage that read two \
             lines (it yields {LINES}): the pipe collected the upstream instead of \
             attaching its body"
        );
    }
}

#[test]
fn a_body_streams_through_a_middle_stage() {
    let polled = Arc::new(AtomicUsize::new(0));
    let engine = Engine::new(kernel(&polled));
    let answer = output(
        &engine,
        "source urn:test:count from=5 | urn:test:relay | urn:test:head",
    );
    assert_eq!(answer.as_deref(), Ok("line 5\nline 6\n"));
}

#[test]
fn a_sink_that_reads_its_body_polls_the_upstream_only_as_far_as_it_reads() {
    let polled = Arc::new(AtomicUsize::new(0));
    let kept = Arc::new(Mutex::new(Vec::new()));
    let engine = Engine::new(Kernel::with_meta_renderer(
        Arc::new(space(&polled, &kept)),
        Arc::new(JsonRenderer),
    ));
    let answer = output(
        &engine,
        "source urn:test:count from=3 | sink urn:test:store",
    );
    assert_eq!(answer.as_deref(), Ok("stored 14 bytes"));
    assert_eq!(kept.lock().unwrap().as_slice(), b"line 3\nline 4\n");
    let polled = polled.load(Ordering::SeqCst);
    assert!(polled <= 2, "the sink's upstream was polled {polled} times");
}

#[test]
fn an_undeclared_stage_answers_the_same_bytes_collected_and_the_trace_says_so() {
    let polled = Arc::new(AtomicUsize::new(0));
    let notes = Arc::new(Notes::default());
    let kernel = kernel(&polled);
    kernel.set_tracer(notes.clone());
    let engine = Engine::new(kernel);
    let by_value = output(
        &engine,
        "source urn:test:count from=0 | urn:test:head-bytes",
    );
    assert_eq!(by_value.as_deref(), Ok("line 0\nline 1\n"));
    assert_eq!(polled.load(Ordering::SeqCst), LINES, "collected whole");
    let collected = format!("{COLLECTED_NOTE}=content");
    assert!(
        notes.0.lock().unwrap().contains(&collected),
        "the kernel collected the attached body for the undeclared stage and noted \
         `{collected}`: {:?}",
        notes.0.lock().unwrap()
    );
    // And the declared stage's answer is the same bytes.
    let by_body = output(&engine, "source urn:test:count from=0 | urn:test:head");
    assert_eq!(by_body, by_value);
}

#[test]
fn past_the_byte_budget_the_answer_is_over_budget_never_a_shorter_body() {
    let polled = Arc::new(AtomicUsize::new(0));
    // Room for 64 bytes in a run: the producer yields about 8.9 KB.
    let engine = Engine::new(kernel(&polled).with_stream_memory(64, 64));
    for line in [
        // The kernel collects for the undeclared stage, under the budget.
        "source urn:test:count from=0 | urn:test:head-bytes",
        // The stage collects the body itself, under the same budget.
        "source urn:test:count from=0 | urn:test:all",
        // The printer's final answer is collected under the budget too.
        "source urn:test:count from=0 | urn:test:relay",
    ] {
        let error = output(&engine, line).expect_err("past the budget: a refusal");
        assert!(error.contains("byte budget"), "{line}: {error}");
    }
    // A stage that stops early stays inside the same budget.
    assert_eq!(
        output(&engine, "source urn:test:count from=0 | urn:test:head").as_deref(),
        Ok("line 0\nline 1\n")
    );
}

#[test]
fn a_fork_over_a_one_shot_body_answers_on_every_branch() {
    let polled = Arc::new(AtomicUsize::new(0));
    let engine = Engine::new(kernel(&polled));
    let answer = output(
        &engine,
        "source urn:test:count from=0 | ( urn:test:head ; urn:test:head-bytes ; urn:test:head )",
    );
    assert_eq!(
        answer.as_deref(),
        Ok("line 0\nline 1\n\nline 0\nline 1\n\nline 0\nline 1\n")
    );
    // And a map: every item is its own value; the final newline terminates
    // the second item rather than adding an empty third one.
    let answer = output(
        &engine,
        "source urn:test:count from=0 | urn:test:head .. urn:test:head",
    );
    assert_eq!(answer.as_deref(), Ok("line 0\nline 1"));
    assert_eq!(engine.last_fan_out().unwrap().nominal, 2);
}

/// A peer reached as a transport reaches one: each call resolved on another thread, the
/// answer collected and returned whole. Only the methods a mount calls, the rest default.
struct OffThread(Arc<Kernel>);
impl Resolver for OffThread {
    fn issue(&self, request: Request) -> Result<(Representation, CacheStatus)> {
        self.issue_as(request, &Capability::root())
    }
    fn issue_as(
        &self,
        request: Request,
        capability: &Capability,
    ) -> Result<(Representation, CacheStatus)> {
        let (peer, capability) = (self.0.clone(), capability.clone());
        std::thread::spawn(move || peer.issue_as(request, &capability))
            .join()
            .expect("the peer's thread")
    }
    fn is_cached(&self, _: &Request, _: &Capability) -> bool {
        false
    }
    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        self.0.entries()
    }
}

/// The local one-shot producer, and every other name reached through a mount of `peer`.
struct LocalCountOverMount {
    count: EndpointSpace,
    mount: RemoteSpace,
}
impl Space for LocalCountOverMount {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        match request.target.as_str() {
            "urn:test:count" => self.count.resolve(request, scope),
            _ => self.mount.resolve(request, scope),
        }
    }
}

#[test]
fn a_mounted_stage_still_answers_on_the_collected_path() {
    // The peer binds every endpoint; `urn:test:head` and `urn:test:relay` declare
    // `reads_body` here too, by their peer's contract. The producer is local and one-shot,
    // so the pipe attaches its body to a request a mount forwards: the mount collects it
    // and sends the bytes, since a content id does not cross a wire.
    let peer = Arc::new(OffThread(Arc::new(kernel(&Arc::new(AtomicUsize::new(0))))));
    let polled = Arc::new(AtomicUsize::new(0));
    let local = Kernel::with_meta_renderer(
        Arc::new(LocalCountOverMount {
            count: EndpointSpace::new().bind(
                Exact::new("urn:test:count"),
                Counting {
                    polled: polled.clone(),
                },
            ),
            mount: RemoteSpace::new(peer),
        }),
        Arc::new(JsonRenderer),
    );
    let engine = Engine::new(local);
    for line in [
        "source urn:test:count from=0 | urn:test:head",
        "source urn:test:count from=0 | urn:test:head-bytes",
        "source urn:test:count from=0 | urn:test:relay | urn:test:head",
        "source urn:test:count | urn:test:head",
    ] {
        assert_eq!(
            output(&engine, line).as_deref(),
            Ok("line 0\nline 1\n"),
            "{line}"
        );
    }
}
