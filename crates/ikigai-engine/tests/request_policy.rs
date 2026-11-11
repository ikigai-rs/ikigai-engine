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
type Roles = Arc<Mutex<Vec<(Request, bool)>>>;

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
    roles: Roles,
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
        self.roles
            .lock()
            .unwrap()
            .push((request.clone(), context.is_output));
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

    fn is_cached(&self, request: &Request, capability: &Capability) -> bool {
        self.kernel.is_cached(request, capability)
    }

    fn is_cached_in(&self, request: &Request, capability: &Capability, scope: &Scope) -> bool {
        self.kernel.is_cached_in(request, capability, scope)
    }
    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        None
    }
}

fn fixture() -> (Engine, Calls) {
    fixture_with_json(true)
}

fn fixture_with_json(json: bool) -> (Engine, Calls) {
    fixture_with_document(json, "source urn:test:producer")
}

fn fixture_with_document(json: bool, document: &str) -> (Engine, Calls) {
    fixture_recording_roles(json, document, Arc::default())
}

fn fixture_recording_roles(json: bool, document: &str, roles: Roles) -> (Engine, Calls) {
    let document = document.to_string();
    let producer = FnEndpoint::new("producer", |inv: &Invocation<'_>| {
        let face = inv.inline_str("as").unwrap_or("text/html");
        Ok(Representation::new(ReprType::new(face), face.as_bytes().to_vec()).cacheable())
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
        .bind(Exact::new("urn:test:stream"), StreamProducer)
        .bind(
            Exact::new("urn:lisp:eval"),
            FnEndpoint::new("eval", |inv: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    inv.inline_arg("in")?.to_vec(),
                ))
            })
            .with_description(
                Description::new("eval")
                    .input(ArgSpec::new("in"))
                    .output("text/plain"),
            ),
        )
        .bind(
            Exact::new("urn:plan:eval"),
            FnEndpoint::new("plan-eval", |_: &Invocation<'_>| {
                unreachable!("the binding only reads the contract")
            })
            .with_description(Description::new("plan-eval").input(ArgSpec::new("in"))),
        )
        .bind(
            Exact::new("urn:test:names"),
            FnEndpoint::new("names", |_: &Invocation<'_>| {
                Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
            }),
        )
        .bind(
            Exact::new("urn:test:document"),
            FnEndpoint::new("document", move |inv: &Invocation<'_>| {
                let value = if inv.inline_str("as").ok() == Some("text/plain") {
                    "presentation, not executable source"
                } else {
                    &document
                };
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    value.as_bytes().to_vec(),
                ))
            })
            .with_description(Description::new("document").output("text/plain")),
        );
    let calls = Arc::new(Mutex::new(Vec::new()));
    (
        Engine::new(Terminal {
            kernel: if json {
                Kernel::with_meta_renderer(Arc::new(space), Arc::new(Json))
            } else {
                Kernel::new(Arc::new(space))
            },
            calls: calls.clone(),
            roles,
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
            (
                "source ( ( urn:test:producer a ; urn:test:producer b ) | urn:test:consumer ; urn:test:producer c )",
                "text/html\ntext/html\ntext/plain",
            ),
            (
                "source urn:test:consumer in=\"a\nb\" .. ( urn:test:producer ; urn:test:producer )",
                "text/plain\ntext/plain\ntext/plain\ntext/plain",
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

#[test]
fn cache_probe_uses_the_face_that_source_will_request() {
    for suffix in ["", " input"] {
        let (engine, _) = fixture();
        let spec = format!("urn:test:producer{suffix}");
        assert_eq!(output(&engine, &format!("cache {spec}")), "not cached");
        assert_eq!(output(&engine, &format!("source {spec}")), "text/plain");
        assert_eq!(output(&engine, &format!("cache {spec}")), "cached");
        // A different explicit face still has its own cache identity.
        assert_eq!(
            output(&engine, &format!("cache {spec} as=text/html")),
            "not cached"
        );
    }
}

#[test]
fn a_stored_script_is_an_internal_read() {
    let (engine, _) = fixture();
    assert_eq!(output(&engine, "script urn:test:document"), "text/plain");
}

#[test]
fn a_lisp_load_is_an_internal_read() {
    let (engine, calls) = fixture_with_document(true, "(+ 1 2)");
    assert_eq!(output(&engine, ":load urn:test:document"), "(+ 1 2)");
    let calls = calls.lock().unwrap();
    let load = calls
        .iter()
        .find(|r| r.verb == Verb::Source && r.target.as_str() == "urn:test:document")
        .unwrap();
    assert!(!load.args.contains_key("as"));
    let eval = calls
        .iter()
        .find(|r| r.verb == Verb::Source && r.target.as_str() == "urn:lisp:eval")
        .unwrap();
    assert!(
        eval.args.contains_key("as"),
        "the evaluated answer is displayed"
    );
}

#[cfg(feature = "plan-reader")]
#[test]
fn a_stored_plans_loading_pipeline_is_internal() {
    for spec in ["urn:test:document", "urn:test:document | urn:test:consumer"] {
        let (engine, _) = fixture();
        let plan = output(&engine, "plan urn:test:producer | sink urn:test:document");
        let (engine, calls) = fixture_with_document(true, &plan);
        assert_eq!(output(&engine, &format!("execute {spec}")), plan);
        let calls = calls.lock().unwrap();
        assert!(calls
            .iter()
            .filter(|r| r.verb == Verb::Source)
            .all(|r| !r.args.contains_key("as")));
    }
}

#[cfg(feature = "plan-reader")]
#[test]
fn a_binding_reads_plan_parameters_without_output_policy() {
    let (engine, _) = fixture();
    let plan = output(&engine, "plan urn:test:producer | sink urn:test:document");
    let (engine, _) = fixture_with_document(true, &plan);
    let engine = engine.with_names_atom(ikigai_core::Iri::parse("urn:test:names").unwrap());
    assert!(output(
        &engine,
        "bind urn:test:bound = source urn:plan:eval in=<urn:test:document>"
    )
    .starts_with("bound in"));
}

#[test]
fn a_stage_after_a_sink_is_output_and_the_stage_before_it_is_internal() {
    let (engine, calls) = fixture();
    assert_eq!(
        output(
            &engine,
            "source urn:test:producer input | sink urn:test:document | urn:test:producer"
        ),
        "text/plain"
    );
    let calls = calls.lock().unwrap();
    let producers: Vec<_> = calls
        .iter()
        .filter(|r| r.verb == Verb::Source && r.target.as_str() == "urn:test:producer")
        .collect();
    assert_eq!(producers.len(), 2);
    assert!(!producers[0].args.contains_key("as"));
    assert!(producers[1].args.contains_key("as"));
}

// A policy can return a different request target; trace must describe what was issued.
struct Retarget {
    kernel: Kernel,
    trace: bool,
}

#[async_trait::async_trait]
impl Resolver for Retarget {
    fn issue(&self, request: Request) -> ikigai_core::Result<(Representation, CacheStatus)> {
        Resolver::issue(&self.kernel, request)
    }

    async fn issue_as_async_in(
        &self,
        request: Request,
        capability: &Capability,
        incoming: Option<Provenance>,
        scope: Scope,
    ) -> ikigai_core::Result<(Representation, CacheStatus)> {
        self.kernel
            .issue_as_async_in(request, capability, incoming, scope)
            .await
    }

    fn needs_request_contract(&self, request: &Request, _: bool) -> bool {
        request.verb == Verb::Source
    }

    fn prepare_request(&self, mut request: Request, context: RequestContext<'_>) -> Request {
        assert!(
            context.contract.is_some(),
            "Arc must forward the contract opt-in"
        );
        request.target = ikigai_core::Iri::parse("urn:test:after").unwrap();
        request
    }

    fn is_cached(&self, request: &Request, capability: &Capability) -> bool {
        self.kernel.is_cached(request, capability)
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        Resolver::entries(&self.kernel)
    }

    fn set_tracer(&self, tracer: Arc<dyn ikigai_core::Tracer>) {
        if self.trace {
            self.kernel.set_tracer(tracer);
        }
    }

    fn clear_tracer(&self) {
        self.kernel.clear_tracer();
    }
}

fn retargeted_trace(trace: bool) -> String {
    let endpoint = || {
        FnEndpoint::new("answer", |_: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new("text/plain"),
                b"answer".to_vec(),
            ))
        })
    };
    let space = EndpointSpace::new()
        .bind(Exact::new("urn:test:before"), endpoint())
        .bind(Exact::new("urn:test:after"), endpoint());
    // Arc must forward both policy hooks, including the opt-in contract lookup.
    let engine = Engine::new(Arc::new(Retarget {
        kernel: Kernel::with_meta_renderer(Arc::new(space), Arc::new(Json)),
        trace,
    }));
    output(&engine, "trace urn:test:before")
}

#[test]
fn trace_describes_the_prepared_target() {
    for trace in [false, true] {
        let result = retargeted_trace(trace);
        assert!(result.starts_with("trace  urn:test:after\n"), "{result}");
        assert!(result.contains("answer"), "{result}");
    }
}

#[test]
fn trace_excludes_policy_contract_reads() {
    let result = retargeted_trace(true);
    assert!(
        result
            .lines()
            .skip(1)
            .all(|line| !line.contains("urn:test:before")),
        "{result}"
    );
}

#[test]
fn a_bindings_discarded_write_reply_is_internal() {
    let roles = Roles::default();
    let (engine, _) = fixture_recording_roles(true, "", roles.clone());
    let engine = engine.with_names_atom(ikigai_core::Iri::parse("urn:test:names").unwrap());
    assert!(
        output(&engine, "bind urn:test:bound = source urn:test:producer").starts_with("bound in")
    );
    let roles = roles.lock().unwrap();
    let (_, is_output) = roles
        .iter()
        .find(|(r, _)| r.verb == Verb::Sink && r.target.as_str() == "urn:test:names")
        .unwrap();
    assert!(
        !is_output,
        "bind discards the atom's reply and formats its own result"
    );
}

#[test]
fn diagnostic_reads_parsed_by_the_engine_are_internal() {
    let roles = Roles::default();
    let (engine, _) = fixture_recording_roles(true, "", roles.clone());
    output(&engine, "why urn:test:producer");
    output(&engine, "explain urn:test:producer");
    let roles = roles.lock().unwrap();
    let internal: Vec<_> = roles
        .iter()
        .filter(|(r, _)| {
            matches!(
                r.target.as_str(),
                "urn:kernel:cached" | "urn:kernel:uncached"
            )
        })
        .collect();
    assert_eq!(internal.len(), 3);
    assert!(
        internal.iter().all(|(_, is_output)| !is_output),
        "{internal:?}"
    );
    assert!(roles
        .iter()
        .any(|(r, is_output)| r.target.as_str() == "urn:kernel:explain" && *is_output));
}
