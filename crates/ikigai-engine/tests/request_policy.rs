//! A terminal policy must reuse routing Meta and leave upstream values alone.
use std::sync::{Arc, Mutex};

use ikigai_core::{
    ArgRef, ArgSpec, AsyncFnEndpoint, Body, BoxFuture, Capability, Description, Endpoint,
    EndpointSpace, Exact, FnEndpoint, Invocation, Kernel, MetaRenderer, Provenance, ReprType,
    Representation, Request, Scope, SpaceEntry, Spawner, Verb,
};
use ikigai_engine::engine::{Action, Engine};
use ikigai_resolve::{CacheStatus, RequestContext, Resolver};

type Calls = Arc<Mutex<Vec<Request>>>;

struct Json;
impl MetaRenderer for Json {
    fn render(
        &self,
        description: &Description,
        _: &ReprType,
    ) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(description).unwrap(),
        ))
    }
}

struct Terminal {
    kernel: Kernel,
    calls: Calls,
}

#[async_trait::async_trait]
impl Resolver for Terminal {
    fn issue(&self, _: Request) -> ikigai_core::Result<(Representation, CacheStatus)> {
        unreachable!("engine uses async issue")
    }

    async fn issue_as_async_in(
        &self,
        request: Request,
        capability: &Capability,
        incoming: Option<Provenance>,
        scope: Scope,
    ) -> ikigai_core::Result<(Representation, CacheStatus)> {
        self.calls.lock().unwrap().push(request.clone());
        self.kernel
            .issue_as_async_in(request, capability, incoming, scope)
            .await
    }

    async fn issue_attached_in(
        &self,
        request: Request,
        capability: &Capability,
        incoming: Option<Provenance>,
        scope: Scope,
        attach: Option<(String, Body)>,
    ) -> ikigai_core::Result<(Representation, CacheStatus)> {
        self.calls.lock().unwrap().push(request.clone());
        self.kernel
            .issue_attached_in(request, capability, incoming, scope, attach)
            .await
    }

    async fn issue_body_attached_in(
        &self,
        request: Request,
        capability: &Capability,
        incoming: Option<Provenance>,
        scope: Scope,
        attach: Option<(String, Body)>,
    ) -> ikigai_core::Result<(Representation<Body>, CacheStatus)> {
        self.calls.lock().unwrap().push(request.clone());
        self.kernel
            .issue_body_attached_in(request, capability, incoming, scope, attach)
            .await
    }

    fn needs_request_contract(&self, request: &Request, is_output: bool) -> bool {
        is_output && request.verb == Verb::Source && !request.args.contains_key("as")
    }

    fn prepare_request(&self, mut request: Request, context: RequestContext<'_>) -> Request {
        if context.is_output
            && request.verb == Verb::Source
            && !request.args.contains_key("as")
            && context
                .contract
                .is_some_and(|d| d.outputs.iter().any(|face| face == "text/plain"))
        {
            request = request.with_arg("as", ArgRef::Inline(b"text/plain".to_vec()));
        }
        request
    }

    fn is_cached(&self, _: &Request, _: &Capability) -> bool {
        false
    }
    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        None
    }
}

fn fixture() -> (Engine, Calls) {
    fixture_with_json(true)
}

fn fixture_with_json(json: bool) -> (Engine, Calls) {
    let producer = FnEndpoint::new("producer", |inv: &Invocation<'_>| {
        let face = inv.inline_str("as").unwrap_or("text/html");
        Ok(Representation::new(
            ReprType::new(face),
            face.as_bytes().to_vec(),
        ))
    })
    .with_description(
        Description::new("producer")
            .input(ArgSpec::new("in").optional())
            .output("text/html")
            .output("text/plain"),
    );
    let consumer = FnEndpoint::new("consumer", |inv: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            inv.inline_arg("in")?.to_vec(),
        ))
    })
    .with_description(
        Description::new("consumer")
            .input(ArgSpec::new("in"))
            .output("text/plain"),
    );
    let reference = AsyncFnEndpoint::new("reference", |inv| {
        Box::pin(async move {
            let body = inv.content_body("content").await?;
            Ok(Representation::new(
                ReprType::new("text/plain"),
                inv.collect(body).await?.to_vec(),
            ))
        })
    })
    .with_description(
        Description::new("reference")
            .input(ArgSpec::new("content"))
            .transreptor(["text/html", "text/plain"], ["text/plain"])
            .output("text/plain")
            .incremental(),
    );
    let space = EndpointSpace::new()
        .bind(Exact::new("urn:test:producer"), producer)
        .bind(Exact::new("urn:test:consumer"), consumer)
        .bind(Exact::new("urn:test:reference"), reference)
        .bind(Exact::new("urn:test:stream"), StreamProducer);
    let calls = Arc::new(Mutex::new(Vec::new()));
    (
        Engine::new(Terminal {
            kernel: if json {
                Kernel::with_meta_renderer(Arc::new(space), Arc::new(Json))
            } else {
                Kernel::new(Arc::new(space))
            },
            calls: calls.clone(),
        }),
        calls,
    )
}

fn output(engine: &Engine, line: &str) -> String {
    match engine.eval(line) {
        Action::Output(entry) => entry.result.unwrap(),
        _ => panic!("expected output"),
    }
}

#[test]
fn terminal_policy_reuses_the_routing_contract() {
    let (engine, calls) = fixture();
    assert_eq!(
        output(&engine, "source urn:test:producer input"),
        "text/plain"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|r| r.verb == Verb::Meta).count(), 1);
}

#[test]
fn terminal_policy_does_not_change_an_intermediate_face() {
    let (engine, _) = fixture();
    assert_eq!(
        output(
            &engine,
            "source urn:test:producer input | urn:test:consumer"
        ),
        "text/html"
    );
}

// A real one-shot body exercises the attachment path, rather than only Body::Bytes.
struct StreamProducer;
#[async_trait::async_trait]
impl Endpoint for StreamProducer {
    async fn invoke(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        self.invoke_body(inv)
            .await?
            .collect_within(inv.memory_budget().unwrap())
            .await
    }
    async fn invoke_body(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation<Body>> {
        let face = inv.inline_str("as").unwrap_or("text/html").to_string();
        Ok(Representation::from_body(
            ReprType::new(&face),
            Body::once(futures::stream::once(async move {
                Ok(ikigai_core::Bytes::from(face))
            })),
        ))
    }
    fn name(&self) -> &str {
        "stream"
    }
    fn describe(&self) -> Description {
        Description::new("stream")
            .output("text/html")
            .output("text/plain")
    }
}

struct Inline;
impl Spawner for Inline {
    fn spawn(&self, task: BoxFuture<()>) -> BoxFuture<()> {
        task
    }
}

#[test]
fn a_bare_output_gets_one_contract_and_each_command_gets_a_fresh_lookup() {
    let (engine, calls) = fixture();
    for _ in 0..2 {
        assert_eq!(output(&engine, "source urn:test:producer"), "text/plain");
    }
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.verb == Verb::Meta)
            .count(),
        2
    );
}

#[test]
fn an_explicit_face_wins() {
    let (engine, _) = fixture();
    assert_eq!(
        output(&engine, "source urn:test:producer as=text/html"),
        "text/html"
    );
}

#[test]
fn value_and_reference_pipes_preserve_the_same_upstream_face() {
    for spec in [
        "source urn:test:producer | urn:test:reference",
        "source urn:test:producer input | urn:test:reference",
        "source urn:test:producer | urn:test:reference | urn:test:consumer",
    ] {
        let (engine, calls) = fixture();
        assert_eq!(output(&engine, spec), "text/html", "{spec}");
        let calls = calls.lock().unwrap();
        let reference = calls
            .iter()
            .find(|r| r.verb == Verb::Source && r.target.as_str() == "urn:test:reference")
            .unwrap();
        assert_eq!(
            reference.args.contains_key("as"),
            !spec.ends_with("consumer")
        );
        if !spec.contains("input") {
            assert!(matches!(
                reference.args.get("content"),
                Some(ArgRef::Reference(_))
            ));
        }
        // The optimizer's contract is reused for routing and presentation.
        assert_eq!(
            calls
                .iter()
                .filter(|r| r.verb == Verb::Meta && r.target.as_str() == "urn:test:reference")
                .count(),
            1
        );
    }
}

#[test]
fn streaming_attachments_keep_upstream_policy_off_and_final_policy_on() {
    let (engine, calls) = fixture();
    assert_eq!(
        output(
            &engine,
            "source urn:test:stream | urn:test:consumer | urn:test:consumer"
        ),
        "text/html"
    );
    let calls = calls.lock().unwrap();
    let sources: Vec<_> = calls.iter().filter(|r| r.verb == Verb::Source).collect();
    assert!(!sources[0].args.contains_key("as"));
    assert!(!sources[1].args.contains_key("as"));
    assert!(sources[2].args.contains_key("as"));
}

#[test]
fn forks_and_maps_propagate_output_role_with_and_without_a_spawner() {
    for parallel in [false, true] {
        for (spec, expected) in [
            (
                "source ( urn:test:producer a ; urn:test:producer b )",
                "text/plain\ntext/plain",
            ),
            (
                "source ( urn:test:producer a ; urn:test:producer b ) | urn:test:consumer",
                "text/html\ntext/html",
            ),
            (
                "source urn:test:consumer in=\"a\nb\" .. urn:test:producer",
                "text/plain\ntext/plain",
            ),
            (
                "source urn:test:consumer in=\"a\nb\" .. urn:test:producer | urn:test:consumer",
                "text/html\ntext/html",
            ),
            (
                "source ( urn:test:producer a | urn:test:consumer ; urn:test:producer b )",
                "text/html\ntext/plain",
            ),
        ] {
            let (mut engine, _) = fixture();
            if parallel {
                engine = engine.with_spawner(Arc::new(Inline));
            }
            assert_eq!(output(&engine, spec), expected, "{parallel}: {spec}");
        }
    }
}

#[test]
fn an_unavailable_routing_contract_is_not_fetched_again_for_policy() {
    for spec in ["source urn:test:producer input", "source urn:test:producer"] {
        let (engine, calls) = fixture_with_json(false);
        assert_eq!(output(&engine, spec), "text/html");
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.verb == Verb::Meta)
                .count(),
            1
        );
        if spec.ends_with("producer") {
            assert!(
                engine.last_contract_notes().is_empty(),
                "policy reads did not route arguments"
            );
        } else {
            assert_eq!(engine.last_contract_notes().len(), 1);
        }
    }
}

#[test]
fn policy_reads_the_contract_in_the_same_scope_as_the_answer() {
    for spec in ["source urn:test:producer", "source urn:test:producer input"] {
        let (engine, calls) = fixture();
        let scoped = FnEndpoint::new("scoped", |inv| {
            Ok(Representation::new(
                ReprType::new("text/plain"),
                inv.inline_str("as")
                    .unwrap_or("scoped-html")
                    .as_bytes()
                    .to_vec(),
            ))
        })
        .with_description(
            Description::new("scoped")
                .input(ArgSpec::new("in").optional())
                .output("text/html"),
        );
        let scope = Scope::empty().with_named(
            ikigai_core::Iri::parse("urn:test:scope").unwrap(),
            Arc::new(EndpointSpace::new().bind(Exact::new("urn:test:producer"), scoped)),
        );
        let engine = engine.with_scope(scope);
        output(&engine, "cap urn:cap:read");
        assert_eq!(output(&engine, spec), "scoped-html");
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.verb == Verb::Meta)
                .count(),
            1
        );
    }
}
