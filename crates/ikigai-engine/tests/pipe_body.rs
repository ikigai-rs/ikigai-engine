//! A pipe hands a stage that reads its `content` as a body the upstream BY REFERENCE
//! (ledger #2286): the stage reads the upstream's body as far as it wants, and a one-shot
//! producer is polled only that far. Every other stage keeps the by-value pipe.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, AsyncFnEndpoint, Body, Bytes, Description, Endpoint, EndpointSpace, Exact, FnEndpoint,
    Invocation, Kernel, MetaRenderer, ReprType, Representation, Result,
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

/// A one-shot producer of `LINES` lines, one chunk each, counting every chunk polled
/// from it.
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

    async fn invoke_body(&self, _inv: &Invocation<'_>) -> Result<Representation<Body>> {
        let polled = self.polled.clone();
        let chunks = futures::stream::iter(0..LINES).map(move |n| {
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
}

use futures::StreamExt;

/// The first two lines of `text` (each with its newline).
fn first_two(text: &[u8]) -> Vec<u8> {
    text.split_inclusive(|b| *b == b'\n')
        .take(2)
        .flatten()
        .copied()
        .collect()
}

/// `urn:test:take`: the first two lines of `content`, read as a body and dropped after
/// them. Declared an incremental transreptor, the one opt-in core gives a stage to say it
/// reads its `content` as it comes.
fn take_incremental() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("take", |inv| {
        Box::pin(async move {
            let mut stream = inv.content_body("content").await?.into_stream();
            let mut held = Vec::new();
            while held.iter().filter(|b| **b == b'\n').count() < 2 {
                match stream.next().await {
                    Some(chunk) => held.extend_from_slice(&chunk?),
                    None => break,
                }
            }
            Ok(Representation::new(
                ReprType::new("text/plain"),
                first_two(&held),
            ))
        })
    })
    .with_description(
        Description::new("take")
            .input(ArgSpec::new("content"))
            .transreptor(["text/plain"], ["text/plain"])
            .incremental(),
    )
}

/// `urn:test:take-bytes`: the same answer from the inline bytes, declaring nothing about
/// bodies.
fn take_bytes() -> FnEndpoint {
    FnEndpoint::new("take-bytes", |inv| {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            first_two(inv.inline_arg("content")?),
        ))
    })
    .with_description(Description::new("take-bytes").input(ArgSpec::new("content")))
}

/// `urn:test:all`: the whole of `content`, read as a body and collected under the run's
/// byte budget. Cacheable for as long as what it read is.
fn all_incremental() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("all", |inv| {
        Box::pin(async move {
            let body = inv.content_body("content").await?;
            Ok(Representation::new(
                ReprType::new("text/plain"),
                inv.collect(body).await?.to_vec(),
            )
            .cacheable())
        })
    })
    .with_description(
        Description::new("all")
            .input(ArgSpec::new("content"))
            .transreptor(["text/plain"], ["text/plain"])
            .incremental(),
    )
}

fn kernel(polled: &Arc<AtomicUsize>) -> Kernel {
    let doc: Vec<u8> = (0..LINES)
        .flat_map(|n| format!("line {n}\n").into_bytes())
        .collect();
    let space = EndpointSpace::new()
        .bind(
            Exact::new("urn:test:count"),
            Counting {
                polled: polled.clone(),
            },
        )
        .bind(
            Exact::new("urn:test:doc"),
            FnEndpoint::new("doc", move |_| {
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    doc.clone(),
                ))
            }),
        )
        .bind(Exact::new("urn:test:take"), take_incremental())
        .bind(Exact::new("urn:test:take-bytes"), take_bytes())
        .bind(Exact::new("urn:test:all"), all_incremental());
    Kernel::with_meta_renderer(Arc::new(space), Arc::new(JsonRenderer))
}

fn output(engine: &Engine, line: &str) -> std::result::Result<String, String> {
    match engine.eval(line) {
        Action::Output(entry) => entry.result,
        _ => panic!("`{line}` should produce output"),
    }
}

#[test]
fn an_incremental_stage_polls_its_upstream_only_as_far_as_it_reads() {
    let polled = Arc::new(AtomicUsize::new(0));
    let engine = Engine::new(kernel(&polled));
    let answer = output(&engine, "source urn:test:count | urn:test:take");
    assert_eq!(answer.as_deref(), Ok("line 0\nline 1\n"));
    let polled = polled.load(Ordering::SeqCst);
    assert!(
        polled <= 2,
        "the producer was polled {polled} times for a stage that read two lines \
         (it yields {LINES}): the pipe collected the upstream instead of handing it on"
    );
}

#[test]
fn the_body_answer_equals_the_bytes_answer() {
    let polled = Arc::new(AtomicUsize::new(0));
    let engine = Engine::new(kernel(&polled));
    for upstream in ["urn:test:count", "urn:test:doc"] {
        let by_body = output(&engine, &format!("source {upstream} | urn:test:take"));
        let by_value = output(&engine, &format!("source {upstream} | urn:test:take-bytes"));
        assert_eq!(by_body, by_value, "upstream {upstream}");
        assert_eq!(by_body.as_deref(), Ok("line 0\nline 1\n"));
        let whole = output(&engine, &format!("source {upstream} | urn:test:all"));
        let direct = output(&engine, &format!("source {upstream}"));
        assert_eq!(whole, direct, "upstream {upstream}, collected whole");
    }
}

#[test]
fn a_stage_that_reads_inline_bytes_keeps_the_by_value_pipe() {
    let polled = Arc::new(AtomicUsize::new(0));
    let engine = Engine::new(kernel(&polled));
    let answer = output(&engine, "source urn:test:count | urn:test:take-bytes");
    assert_eq!(answer.as_deref(), Ok("line 0\nline 1\n"));
    assert_eq!(polled.load(Ordering::SeqCst), LINES, "collected, as before");
}

#[test]
fn past_the_byte_budget_the_pipe_is_refused_never_truncated() {
    let polled = Arc::new(AtomicUsize::new(0));
    // Room for 64 bytes in a run: the producer yields about 8.9 KB.
    let engine = Engine::new(kernel(&polled).with_stream_memory(64, 64));
    let answer = output(&engine, "source urn:test:count | urn:test:all");
    let error = answer.expect_err("past the budget the answer is a refusal");
    assert!(error.contains("byte budget"), "{error}");
    // A stage that stops early stays inside the same budget.
    assert_eq!(
        output(&engine, "source urn:test:count | urn:test:take").as_deref(),
        Ok("line 0\nline 1\n")
    );
}

#[test]
fn a_batch_with_piped_stdin_keeps_the_by_value_pipe() {
    let polled = Arc::new(AtomicUsize::new(0));
    let engine = Engine::new(kernel(&polled));
    // A batch's stdin may be the first stage's value, so that stage is resolved as it
    // always was, and its answer piped by value.
    engine.set_piped_input(b"unused".to_vec());
    let answer = output(&engine, "source urn:test:count | urn:test:take");
    assert_eq!(answer.as_deref(), Ok("line 0\nline 1\n"));
    assert_eq!(polled.load(Ordering::SeqCst), LINES);
}

#[test]
fn a_stage_read_by_reference_is_cut_with_its_upstream() {
    let version = Arc::new(std::sync::Mutex::new("v1"));
    let state = version.clone();
    let space = EndpointSpace::new()
        .bind(
            Exact::new("urn:test:versioned"),
            FnEndpoint::new("versioned", move |_| {
                let text = *state.lock().unwrap();
                Ok(
                    Representation::new(ReprType::new("text/plain"), text.as_bytes().to_vec())
                        .cacheable(),
                )
            }),
        )
        .bind(Exact::new("urn:test:all"), all_incremental());
    let kernel = Arc::new(Kernel::with_meta_renderer(
        Arc::new(space),
        Arc::new(JsonRenderer),
    ));
    let engine = Engine::new(kernel.clone());
    let line = "source urn:test:versioned | urn:test:all";
    assert_eq!(output(&engine, line).as_deref(), Ok("v1"));
    *version.lock().unwrap() = "v2";
    // Cached: the state changed and nothing said so yet.
    assert_eq!(output(&engine, line).as_deref(), Ok("v1"));
    // Cutting the upstream's thread recomputes the stage that read it by reference: the
    // kernel recorded the reference as its dependency.
    kernel.cut("urn:test:versioned");
    assert_eq!(output(&engine, line).as_deref(), Ok("v2"));
}
