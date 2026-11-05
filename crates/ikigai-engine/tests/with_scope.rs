//! `Engine::with_scope` (ledger #3081): a host's corridor around every request the engine
//! issues, so a door no longer wraps the `Resolver` to hold one, and a wrapper that missed a
//! method no longer resolves silently outside it.
//!
//! One test per issue path, so a new path without a test here is visible. Every test runs
//! through [`Recording`], a real kernel behind a `Resolver` that logs the method, verb, target
//! and chain of each call the ENGINE makes, so a test proves two things at once: the answer
//! came from the corridor's binding, and no call on the path left the chain (a call on a
//! method without a chain is logged as `UNSCOPED`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ArgSpec, AsyncFnEndpoint, Body, Bytes, Capability, Description, Endpoint, EndpointSpace, Error,
    Exact, FixedClock, FnEndpoint, Invocation, Iri, Kernel, MetaRenderer, Provenance, ReprType,
    Representation, Request, Result, Scope, Space, SpaceEntry, Spawner, Tracer, Verb,
};
use ikigai_engine::engine::{Action, Engine, Viewer};
use ikigai_resolve::{CacheStatus, Resolver, RunContext};

/// The host's corridor, as the kernel's chain prints it.
const CORRIDOR: &str = "urn:ctx:player:alice root";
/// The chain of an engine with no scope.
const ROOT: &str = "root";
/// What the recording logs for a call on a method that takes no chain.
const UNSCOPED: &str = "UNSCOPED";

/// 2026-09-25T18:00:00Z, the instant the `as-of=` tests pin.
const PINNED: u64 = 1_790_359_200_000;
const AS_OF: &str = "as-of=2026-09-25T18:00Z";
/// The chain of an `as-of=` line inside the host's corridor: the temporal corridor innermost.
const AS_OF_IN_CORRIDOR: &str = "urn:ctx:time:2026-09-25T18:00:00Z urn:ctx:player:alice root";
/// The kernel's live clock: a different day, so a leak shows in the digits.
const LIVE: u64 = 1_790_512_496_000;

struct JsonRenderer;
impl MetaRenderer for JsonRenderer {
    fn render(&self, description: &Description, _target: &ReprType) -> Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(description).expect("serialize description"),
        ))
    }
}

fn text(body: impl Into<String>) -> Representation {
    Representation::new(ReprType::new("text/plain"), body.into().into_bytes())
}

fn verb_word(verb: &Verb) -> &'static str {
    match verb {
        Verb::Source => "source",
        Verb::Sink => "sink",
        Verb::Exists => "exists",
        Verb::Delete => "delete",
        _ => "other",
    }
}

/// `urn:test:who`: says which binding answered, for which verb, and what it was handed. Each
/// space binds its own, so the answer names the space the request reached. The corridor's
/// alone declares `room`, so the contract the engine routes by shows which one it read.
fn who(label: &'static str) -> FnEndpoint {
    let description = Description::new("who")
        .verb(Verb::Source)
        .verb(Verb::Sink)
        .verb(Verb::Exists)
        .verb(Verb::Delete)
        .verb(Verb::Meta)
        .input(ArgSpec::new("in").optional())
        .input(ArgSpec::new("content").optional());
    let description = if label == "corridor" {
        description.input(ArgSpec::new("room").optional())
    } else {
        description
    };
    FnEndpoint::new("who", move |inv: &Invocation<'_>| {
        let handed = ["in", "content", "room"]
            .iter()
            .filter_map(|name| inv.inline_str(name).ok())
            .collect::<Vec<_>>()
            .join(",");
        Ok(text(format!("{label}:{}:{handed}", verb_word(&inv.request.verb))).cacheable())
    })
    .with_description(description)
}

/// `urn:test:stage`: takes a piped value in its one required argument and says who took it.
fn stage(label: &'static str) -> FnEndpoint {
    FnEndpoint::new("stage", move |inv: &Invocation<'_>| {
        Ok(text(format!("{label}:stage:{}", inv.inline_str("in")?)))
    })
    .with_description(
        Description::new("stage")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(ArgSpec::new("in")),
    )
}

/// `urn:test:now`: reads the time through the invocation, from whichever space it is in.
fn now(label: &'static str) -> FnEndpoint {
    FnEndpoint::new("now", move |inv: &Invocation<'_>| {
        let now = inv.now().map(|t| t.as_millis()).unwrap_or(0);
        Ok(text(format!("{label}:{now}")))
    })
    .with_description(Description::new("now").verb(Verb::Source))
}

/// `urn:test:stream`: a one-shot body, so the stage after it is handed it attached.
struct Stream;

#[async_trait]
impl Endpoint for Stream {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let body = self.invoke_body(inv).await?;
        let budget = inv.memory_budget().expect("the kernel mints one");
        body.collect_within(budget).await
    }

    async fn invoke_body(&self, _inv: &Invocation<'_>) -> Result<Representation<Body>> {
        let chunks = futures::stream::iter([Ok(Bytes::from_static(b"streamed"))]);
        Ok(Representation::from_body(
            ReprType::new("text/plain"),
            Body::once(chunks),
        ))
    }

    fn name(&self) -> &str {
        "stream"
    }

    fn describe(&self) -> Description {
        Description::new("stream").verb(Verb::Source)
    }
}

/// The corridor: its own `urn:test:who`, and names the root does not bind at all.
fn corridor_space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(Exact::new("urn:test:who"), who("corridor"))
        .bind(Exact::new("urn:test:stage"), stage("corridor"))
        .bind(Exact::new("urn:test:now"), now("corridor"))
        .bind(
            Exact::new("urn:test:only-here"),
            FnEndpoint::new("only-here", |_: &Invocation<'_>| {
                Ok(text("only in the corridor"))
            })
            .with_description(Description::new("only-here").verb(Verb::Source)),
        )
        .bind(
            Exact::new("urn:test:incremental"),
            AsyncFnEndpoint::new("incremental", |inv| {
                Box::pin(async move {
                    let reference = match inv.request.args.get("content") {
                        Some(ikigai_core::ArgRef::Reference(iri)) => iri.as_str().to_string(),
                        _ => "not a reference".to_string(),
                    };
                    let budget = inv.memory_budget().expect("the kernel mints one");
                    let read = inv.content_body("content").await?;
                    let read = read.collect_within(budget).await?;
                    Ok(text(format!(
                        "incremental:{reference}:{}",
                        String::from_utf8_lossy(&read)
                    )))
                })
            })
            .with_description(
                Description::new("incremental")
                    .input(ArgSpec::new("content"))
                    .transreptor(["text/plain"], ["text/plain"])
                    .incremental(),
            ),
        )
        .bind(
            Exact::new("urn:test:picky"),
            FnEndpoint::new("picky", |inv: &Invocation<'_>| {
                match inv.inline_str("needs") {
                    Ok(needs) => Err(Error::Endpoint(format!(
                        "urn:test:picky: no configured backend satisfies `{needs}`"
                    ))),
                    Err(_) => Ok(text("picky in the corridor")),
                }
            })
            .with_description(
                Description::new("picky")
                    .verb(Verb::Source)
                    .verb(Verb::Meta)
                    .input(ArgSpec::new("needs").optional())
                    .input(ArgSpec::new("in")),
            ),
        )
}

fn corridor() -> Scope {
    Scope::empty().with_named(
        Iri::parse("urn:ctx:player:alice").unwrap(),
        Arc::new(corridor_space()),
    )
}

fn root_space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(Exact::new("urn:test:who"), who("root"))
        .bind(Exact::new("urn:test:stage"), stage("root"))
        .bind(Exact::new("urn:test:now"), now("root"))
        .bind(Exact::new("urn:test:stream"), Stream)
        .bind(
            Exact::new("urn:test:list"),
            FnEndpoint::new("list", |_: &Invocation<'_>| Ok(text("a\nb"))),
        )
}

fn kernel_over(space: Arc<dyn Space>) -> Arc<Kernel> {
    Arc::new(
        Kernel::with_meta_renderer(space, Arc::new(JsonRenderer))
            .with_clock(Arc::new(FixedClock::at(LIVE))),
    )
}

/// One call the engine made on its resolver.
#[derive(Clone, Debug)]
struct Call {
    method: &'static str,
    verb: Verb,
    target: String,
    chain: String,
}

/// A real kernel behind a `Resolver` that logs what the engine asks of it. It overrides every
/// issue method, the chain-less ones included, so a path that stopped carrying the chain is
/// logged as such rather than falling through a trait default.
struct Recording {
    kernel: Arc<Kernel>,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl Recording {
    fn log(&self, method: &'static str, request: &Request, chain: String) {
        self.calls.lock().unwrap().push(Call {
            method,
            verb: request.verb,
            target: request.target.as_str().to_string(),
            chain,
        });
    }
}

#[async_trait]
impl Resolver for Recording {
    fn issue(&self, request: Request) -> Result<(Representation, CacheStatus)> {
        self.log("issue", &request, UNSCOPED.to_string());
        Resolver::issue(&*self.kernel, request)
    }

    fn issue_as(
        &self,
        request: Request,
        capability: &Capability,
    ) -> Result<(Representation, CacheStatus)> {
        self.log("issue_as", &request, UNSCOPED.to_string());
        Resolver::issue_as(&*self.kernel, request, capability)
    }

    fn issue_as_in_run(
        &self,
        request: Request,
        capability: &Capability,
        run: &RunContext,
    ) -> Result<(Representation, CacheStatus)> {
        self.log("issue_as_in_run", &request, UNSCOPED.to_string());
        Resolver::issue_as_in_run(&*self.kernel, request, capability, run)
    }

    async fn issue_body_as_in_run(
        &self,
        request: Request,
        capability: &Capability,
        run: &RunContext,
    ) -> Result<(Representation<Body>, CacheStatus)> {
        self.log("issue_body_as_in_run", &request, UNSCOPED.to_string());
        Resolver::issue_body_as_in_run(&*self.kernel, request, capability, run).await
    }

    async fn issue_as_async(
        &self,
        request: Request,
        capability: &Capability,
    ) -> Result<(Representation, CacheStatus)> {
        self.log("issue_as_async", &request, UNSCOPED.to_string());
        Resolver::issue_as_async(&*self.kernel, request, capability).await
    }

    async fn issue_as_async_with_incoming(
        &self,
        request: Request,
        capability: &Capability,
        incoming: Provenance,
    ) -> Result<(Representation, CacheStatus)> {
        self.log(
            "issue_as_async_with_incoming",
            &request,
            UNSCOPED.to_string(),
        );
        Resolver::issue_as_async_with_incoming(&*self.kernel, request, capability, incoming).await
    }

    async fn issue_as_async_in(
        &self,
        request: Request,
        capability: &Capability,
        incoming: Option<Provenance>,
        scope: Scope,
    ) -> Result<(Representation, CacheStatus)> {
        self.log("issue_as_async_in", &request, scope.to_string());
        Resolver::issue_as_async_in(&*self.kernel, request, capability, incoming, scope).await
    }

    async fn issue_attached_in(
        &self,
        request: Request,
        capability: &Capability,
        incoming: Option<Provenance>,
        scope: Scope,
        attach: Option<(String, Body)>,
    ) -> Result<(Representation, CacheStatus)> {
        self.log("issue_attached_in", &request, scope.to_string());
        Resolver::issue_attached_in(&*self.kernel, request, capability, incoming, scope, attach)
            .await
    }

    async fn issue_body_attached_in(
        &self,
        request: Request,
        capability: &Capability,
        incoming: Option<Provenance>,
        scope: Scope,
        attach: Option<(String, Body)>,
    ) -> Result<(Representation<Body>, CacheStatus)> {
        self.log("issue_body_attached_in", &request, scope.to_string());
        Resolver::issue_body_attached_in(
            &*self.kernel,
            request,
            capability,
            incoming,
            scope,
            attach,
        )
        .await
    }

    fn is_cached_in(&self, request: &Request, capability: &Capability, scope: &Scope) -> bool {
        self.log("is_cached_in", request, scope.to_string());
        Resolver::is_cached_in(&*self.kernel, request, capability, scope)
    }

    fn is_cached(&self, request: &Request, capability: &Capability) -> bool {
        self.log("is_cached", request, UNSCOPED.to_string());
        Resolver::is_cached(&*self.kernel, request, capability)
    }

    fn set_tracer(&self, tracer: Arc<dyn Tracer>) {
        Resolver::set_tracer(&*self.kernel, tracer);
    }

    fn clear_tracer(&self) {
        Resolver::clear_tracer(&*self.kernel);
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        Resolver::entries(&*self.kernel)
    }
}

struct World {
    engine: Engine,
    calls: Arc<Mutex<Vec<Call>>>,
}

fn doors() -> Arc<dyn Space> {
    Arc::new(EndpointSpace::new().bind(Exact::new("urn:test:only-at-then"), now("doors")))
}

fn engine_over(kernel: Arc<Kernel>) -> (Engine, Arc<Mutex<Vec<Call>>>) {
    let calls: Arc<Mutex<Vec<Call>>> = Arc::default();
    let engine = Engine::new(Recording {
        kernel,
        calls: calls.clone(),
    })
    .with_as_of_doors(doors());
    (engine, calls)
}

/// An engine over the root space, inside the host's corridor.
fn scoped() -> World {
    let (engine, calls) = engine_over(kernel_over(Arc::new(root_space())));
    World {
        engine: engine.with_scope(corridor()),
        calls,
    }
}

/// An engine over the root space, with no scope.
fn bare() -> World {
    let (engine, calls) = engine_over(kernel_over(Arc::new(root_space())));
    World { engine, calls }
}

fn output(engine: &Engine, line: &str) -> std::result::Result<String, String> {
    match engine.eval(line) {
        Action::Output(entry) => entry.result,
        _ => panic!("`{line}` should produce output"),
    }
}

impl World {
    fn line(&self, line: &str) -> std::result::Result<String, String> {
        output(&self.engine, line)
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// Every call since the last [`forget`](Self::forget) was made in `chain`.
    fn assert_every_call_in(&self, chain: &str) {
        let calls = self.calls();
        assert!(!calls.is_empty(), "the path issued nothing");
        for call in &calls {
            assert_eq!(
                call.chain, chain,
                "left the chain: {call:?}\nall: {calls:#?}"
            );
        }
    }

    /// The path made this call, through this method, and every call stayed in the corridor.
    fn assert_path(&self, method: &str, verb: Verb, target: &str) {
        self.assert_every_call_in(CORRIDOR);
        let calls = self.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.method == method && c.verb == verb && c.target == target),
            "no {method} {verb:?} {target} among {calls:#?}"
        );
    }

    fn forget(&self) {
        self.calls.lock().unwrap().clear();
    }
}

// ---- One test per issue path ------------------------------------------------------------

#[test]
fn source_resolves_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world.line("source urn:test:who").unwrap(),
        "corridor:source:"
    );
    world.assert_path("issue_as_async_in", Verb::Source, "urn:test:who");
}

#[test]
fn a_name_only_the_corridor_binds_resolves() {
    let world = scoped();
    assert_eq!(
        world.line("source urn:test:only-here").unwrap(),
        "only in the corridor"
    );
    world.assert_every_call_in(CORRIDOR);
    assert!(bare().line("source urn:test:only-here").is_err());
}

#[test]
fn sink_resolves_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world.line("sink urn:test:who hello").unwrap(),
        "corridor:sink:hello"
    );
    world.assert_path("issue_as_async_in", Verb::Sink, "urn:test:who");
}

#[test]
fn exists_resolves_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world.line("exists urn:test:who").unwrap(),
        "corridor:exists:"
    );
    world.assert_path("issue_as_async_in", Verb::Exists, "urn:test:who");
}

#[test]
fn delete_resolves_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world.line("delete urn:test:who").unwrap(),
        "corridor:delete:"
    );
    world.assert_path("issue_as_async_in", Verb::Delete, "urn:test:who");
}

/// The contract that routes arguments is the corridor's: `room` is declared there and not on
/// the root's binding, which refuses it.
#[test]
fn the_contract_fetch_resolves_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world.line("source urn:test:who room=kitchen").unwrap(),
        "corridor:source:kitchen"
    );
    world.assert_path("issue_as_async_in", Verb::Meta, "urn:test:who");

    let refused = bare().line("source urn:test:who room=kitchen").unwrap_err();
    assert!(refused.contains("room"), "{refused}");
}

/// The first stage of a pipe answers its body unread, for the stage after it.
#[test]
fn a_stage_that_answers_its_body_resolves_in_the_scope() {
    let world = scoped();
    world
        .line("source urn:test:stream | urn:test:stage")
        .unwrap();
    world.assert_path("issue_body_attached_in", Verb::Source, "urn:test:stream");
}

/// The last stage of a pipe is handed the body attached and answers collected.
#[test]
fn an_attached_body_resolves_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world
            .line("source urn:test:stream | urn:test:stage")
            .unwrap(),
        "corridor:stage:streamed"
    );
    world.assert_path("issue_attached_in", Verb::Source, "urn:test:stage");
}

/// A middle stage both takes an attached body and answers its own.
#[test]
fn a_body_attached_to_a_middle_stage_resolves_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world
            .line("source urn:test:stream | urn:test:stage | urn:test:stage")
            .unwrap(),
        "corridor:stage:corridor:stage:streamed"
    );
    world.assert_path("issue_body_attached_in", Verb::Source, "urn:test:stage");
}

#[test]
fn a_body_attached_to_a_sink_resolves_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world
            .line("source urn:test:stream | sink urn:test:who")
            .unwrap(),
        "corridor:sink:streamed"
    );
    world.assert_path("issue_attached_in", Verb::Sink, "urn:test:who");
}

/// `source <iri> | <incremental stage>` runs as one request, handing the stage its upstream by
/// name, and that name is read from inside the chain too: the corridor's `urn:test:who`.
#[test]
fn a_pipe_by_reference_resolves_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world
            .line("source urn:test:who | urn:test:incremental")
            .unwrap(),
        "incremental:urn:test:who:corridor:source:"
    );
    world.assert_path("issue_as_async_in", Verb::Source, "urn:test:incremental");
}

#[test]
fn a_cache_check_probes_the_scope() {
    let kernel = kernel_over(Arc::new(root_space()));
    let (engine, calls) = engine_over(kernel.clone());
    let world = World {
        engine: engine.with_scope(corridor()),
        calls,
    };
    assert_eq!(world.line("cache urn:test:who").unwrap(), "not cached");
    world.assert_path("is_cached_in", Verb::Source, "urn:test:who");

    world.line("source urn:test:who").unwrap();
    assert_eq!(world.line("cache urn:test:who").unwrap(), "cached");

    // The corridor's answer went into the corridor's partition, never the root's.
    let (root_engine, _) = engine_over(kernel);
    assert_eq!(
        output(&root_engine, "cache urn:test:who").unwrap(),
        "not cached"
    );
}

#[test]
fn a_fork_runs_every_branch_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world
            .line("source urn:test:list | ( urn:test:stage ; urn:test:stage )")
            .unwrap(),
        "corridor:stage:a\nb\ncorridor:stage:a\nb"
    );
    world.assert_every_call_in(CORRIDOR);
}

#[test]
fn a_map_runs_every_item_in_the_scope() {
    let world = scoped();
    assert_eq!(
        world
            .line("source urn:test:list .. urn:test:stage")
            .unwrap(),
        "corridor:stage:a\ncorridor:stage:b"
    );
    world.assert_every_call_in(CORRIDOR);
}

/// Runs tasks inline and declares `width`, so the engine takes its concurrent fan-out path.
struct Wide(usize);
impl Spawner for Wide {
    fn spawn(&self, task: ikigai_core::BoxFuture<()>) -> ikigai_core::BoxFuture<()> {
        task
    }
    fn width(&self) -> Option<usize> {
        Some(self.0)
    }
}

#[test]
fn a_spawned_fork_runs_every_branch_in_the_scope() {
    let (engine, calls) = engine_over(kernel_over(Arc::new(root_space())));
    let world = World {
        engine: engine
            .with_scope(corridor())
            .with_spawner(Arc::new(Wide(8))),
        calls,
    };
    assert_eq!(
        world
            .line("source urn:test:list | ( urn:test:stage ; urn:test:stage )")
            .unwrap(),
        "corridor:stage:a\nb\ncorridor:stage:a\nb"
    );
    world.assert_every_call_in(CORRIDOR);
}

/// The retry a fan-out makes when its automatic width hint matched nothing: the second issue of
/// each item stays in the chain too.
#[test]
fn a_spawned_maps_hint_retry_stays_in_the_scope() {
    let (engine, calls) = engine_over(kernel_over(Arc::new(root_space())));
    let world = World {
        engine: engine
            .with_scope(corridor())
            .with_spawner(Arc::new(Wide(8)))
            .with_width_routing(true),
        calls,
    };
    assert_eq!(
        world
            .line("source urn:test:list .. urn:test:picky")
            .unwrap(),
        "picky in the corridor\npicky in the corridor"
    );
    world.assert_every_call_in(CORRIDOR);
    let picky = world
        .calls()
        .iter()
        .filter(|c| c.verb == Verb::Source && c.target == "urn:test:picky")
        .count();
    assert_eq!(picky, 4, "each of two items: the hinted try and the retry");
}

#[test]
fn trace_resolves_in_the_scope_and_names_it() {
    let world = scoped();
    let traced = world.line("trace urn:test:who").unwrap();
    assert!(
        traced.contains(&format!("scope       {CORRIDOR}")),
        "{traced}"
    );
    world.assert_path("issue_as_async_in", Verb::Source, "urn:test:who");
}

#[test]
fn explain_resolves_in_the_scope() {
    let world = scoped();
    world.line("explain urn:test:who").unwrap();
    world.assert_path("issue_as_async_in", Verb::Source, "urn:kernel:explain");
}

#[test]
fn why_resolves_in_the_scope() {
    let world = scoped();
    world.line("why urn:test:who").unwrap();
    world.assert_path("issue_as_async_in", Verb::Source, "urn:kernel:uncached");
}

#[test]
fn dependents_resolves_in_the_scope() {
    let world = scoped();
    // No such thread is an answer about the name, not a failure of the path under test.
    let _ = world.line("dependents urn:test:who");
    world.assert_path("issue_as_async_in", Verb::Source, "urn:kernel:dependents");
}

struct Keeps(Arc<Mutex<String>>);
impl Viewer for Keeps {
    fn show(
        &self,
        target: &str,
        _media: &str,
        bytes: &[u8],
    ) -> std::result::Result<String, String> {
        *self.0.lock().unwrap() = String::from_utf8_lossy(bytes).into_owned();
        Ok(format!("shown {target}"))
    }
}

/// `show` reads the picture through the chain. The corridor's `urn:test:who` is text, and the
/// refusal of a text face is `show`'s own business: what is pinned is where it read from.
#[test]
fn show_resolves_in_the_scope() {
    let (engine, calls) = engine_over(kernel_over(Arc::new(root_space())));
    let kept = Arc::new(Mutex::new(String::new()));
    let world = World {
        engine: engine
            .with_scope(corridor())
            .with_viewer(Arc::new(Keeps(kept))),
        calls,
    };
    let _ = world.line("show urn:test:who");
    world.assert_path("issue_as_async_in", Verb::Source, "urn:test:who");
}

/// `:load` reads its script from the corridor, and `urn:lisp:eval` is issued in it.
#[test]
fn load_and_lisp_resolve_in_the_scope() {
    let world = scoped();
    let _ = world.line(":load urn:test:who");
    world.assert_path("issue_as_async_in", Verb::Source, "urn:test:who");
    world.assert_path("issue_as_async_in", Verb::Source, "urn:lisp:eval");

    world.forget();
    assert!(matches!(
        world.engine.eval(":lisp"),
        Action::Output(ref entry) if entry.result.is_ok()
    ));
    assert!(matches!(world.engine.eval("(+ 1 1)"), Action::Noop));
    let _ = world.engine.eval("");
    world.assert_path("issue_as_async_in", Verb::Source, "urn:lisp:eval");
}

#[test]
fn a_script_runs_every_line_in_the_scope() {
    let world = scoped();
    let run = world.engine.run_script(
        "source urn:test:who\nsink urn:test:who hello\n",
        &BTreeMap::new(),
    );
    assert!(run.succeeded(), "{:?}", run.error);
    world.assert_path("issue_as_async_in", Verb::Source, "urn:test:who");
    world.assert_path("issue_as_async_in", Verb::Sink, "urn:test:who");
}

/// `bind` reads the names atom, checks the target's contract and writes the atom, and the host's
/// corridor holds them all: the atom is bound in no other space.
#[test]
fn bind_reads_and_writes_the_names_atom_in_the_scope() {
    const ATOM: &str = "urn:test:names";
    let held: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
    let atom = {
        let held = held.clone();
        FnEndpoint::new("names", move |inv: &Invocation<'_>| {
            if inv.request.verb == Verb::Sink {
                *held.lock().unwrap() = Some(inv.inline_arg("content")?.to_vec());
                return Ok(text("ok"));
            }
            match held.lock().unwrap().clone() {
                Some(bytes) => Ok(Representation::new(ReprType::new("text/plain"), bytes)),
                None => Err(Error::NotFound(ATOM.to_string())),
            }
        })
        .with_description(
            Description::new("names")
                .verb(Verb::Source)
                .verb(Verb::Sink)
                .input(ArgSpec::new("content").optional()),
        )
    };
    let scope = Scope::empty().with_named(
        Iri::parse("urn:ctx:player:alice").unwrap(),
        Arc::new(corridor_space().bind(Exact::new(ATOM), atom)),
    );
    let (engine, calls) = engine_over(kernel_over(Arc::new(root_space())));
    let world = World {
        engine: engine
            .with_scope(scope)
            .with_names_atom(Iri::parse(ATOM).unwrap()),
        calls,
    };
    world
        .line("bind urn:q:mine = source urn:test:who room=kitchen")
        .unwrap();
    world.assert_path("issue_as_async_in", Verb::Source, ATOM);
    world.assert_path("issue_as_async_in", Verb::Meta, "urn:test:who");
    world.assert_path("issue_as_async_in", Verb::Sink, ATOM);
    assert!(
        held.lock().unwrap().is_some(),
        "the corridor's atom was written"
    );
}

/// A plan stored in the corridor runs there: the read of the plan and every step.
#[cfg(feature = "plan-reader")]
mod stored_plan {
    use super::*;

    fn world() -> (World, Arc<Mutex<String>>) {
        let plan: Arc<Mutex<String>> = Arc::default();
        let held = plan.clone();
        let scope = Scope::empty().with_named(
            Iri::parse("urn:ctx:player:alice").unwrap(),
            Arc::new(
                corridor_space().bind(
                    Exact::new("urn:t:plan"),
                    FnEndpoint::new("plan-store", move |_: &Invocation<'_>| {
                        Ok(Representation::new(
                            ReprType::new("text/turtle"),
                            held.lock().unwrap().clone().into_bytes(),
                        ))
                    })
                    .with_description(Description::new("plan-store").verb(Verb::Source)),
                ),
            ),
        );
        let root = Arc::new(ikigai_core::Fallback::new(vec![
            Arc::new(root_space()) as Arc<dyn Space>,
            Arc::new(ikigai_shacl::space()),
            Arc::new(ikigai_engine::plan_space::space()),
        ]));
        let (engine, calls) = engine_over(kernel_over(root));
        (
            World {
                engine: engine.with_scope(scope),
                calls,
            },
            plan,
        )
    }

    #[test]
    fn execute_runs_every_step_in_the_scope() {
        let (world, plan) = world();
        let turtle = world
            .line("plan urn:test:stream | urn:test:stage | sink urn:test:who")
            .unwrap();
        *plan.lock().unwrap() = turtle;
        world.forget();
        assert_eq!(
            world.line("execute urn:t:plan").unwrap(),
            "corridor:sink:corridor:stage:streamed"
        );
        world.assert_path("issue_as_async_in", Verb::Source, "urn:t:plan");
        world.assert_every_call_in(CORRIDOR);
        let calls = world.calls();
        for (verb, target) in [
            (Verb::Source, "urn:test:stream"),
            (Verb::Source, "urn:test:stage"),
            (Verb::Sink, "urn:test:who"),
        ] {
            assert!(
                calls.iter().any(|c| c.verb == verb && c.target == target),
                "{verb:?} {target}: {calls:#?}"
            );
        }
    }

    /// `urn:plan:eval` is one request, and the sub-requests it gives rise to inherit the chain.
    #[test]
    fn plan_eval_runs_every_step_in_the_scope() {
        let (world, plan) = world();
        *plan.lock().unwrap() = world.line("plan urn:test:who | sink urn:test:who").unwrap();
        world.forget();
        let answer = world.line("source urn:t:plan | urn:plan:eval").unwrap();
        assert_eq!(answer, "corridor:sink:corridor:source:");
        world.assert_path("issue_as_async_in", Verb::Source, "urn:plan:eval");
    }
}

// ---- `as-of=` stacks on the scope ---------------------------------------------------------

/// The temporal corridor goes INSIDE the host's: a name the time doors do not bind still
/// reaches the host's corridor, under the pinned clock, and the next line is back.
#[test]
fn as_of_stacks_on_the_scope() {
    let world = scoped();
    assert_eq!(
        world.line(&format!("source urn:test:now {AS_OF}")).unwrap(),
        format!("corridor:{PINNED}")
    );
    world.assert_every_call_in(AS_OF_IN_CORRIDOR);

    world.forget();
    assert_eq!(
        world.line("source urn:test:now").unwrap(),
        format!("corridor:{LIVE}"),
        "the next line is in the host's corridor and on the live clock"
    );
    world.assert_every_call_in(CORRIDOR);
}

/// A name the time doors bind answers from them, ahead of the host's corridor, and the root's
/// binding is still reached only by way of both.
#[test]
fn as_of_doors_shadow_the_scope_and_the_scope_shadows_the_root() {
    let shadow: Arc<dyn Space> =
        Arc::new(EndpointSpace::new().bind(Exact::new("urn:test:who"), who("doors")));
    let (engine, calls) = engine_over(kernel_over(Arc::new(root_space())));
    let world = World {
        engine: engine.with_scope(corridor()).with_as_of_doors(shadow),
        calls,
    };
    assert_eq!(
        world.line(&format!("source urn:test:who {AS_OF}")).unwrap(),
        "doors:source:"
    );
    assert_eq!(
        world.line("source urn:test:who").unwrap(),
        "corridor:source:"
    );
    assert_eq!(
        world
            .line(&format!("source urn:test:list {AS_OF}"))
            .unwrap(),
        "a\nb"
    );
}

#[test]
fn as_of_cache_checks_probe_the_stacked_partition() {
    let world = scoped();
    let at = format!("cache urn:test:who {AS_OF}");
    assert_eq!(world.line(&at).unwrap(), "not cached");
    world.line("source urn:test:who").unwrap();
    assert_eq!(world.line("cache urn:test:who").unwrap(), "cached");
    assert_eq!(
        world.line(&at).unwrap(),
        "not cached",
        "the host's corridor and the as-of line inside it are two partitions"
    );
    world.line(&format!("source urn:test:who {AS_OF}")).unwrap();
    assert_eq!(world.line(&at).unwrap(), "cached");
    let last = world.calls().last().cloned().unwrap();
    assert_eq!(last.chain, AS_OF_IN_CORRIDOR);
}

/// Every command that takes `as-of=` takes the same stack.
#[test]
fn as_of_stacks_on_every_view_command() {
    for line in [
        format!("exists urn:test:who {AS_OF}"),
        format!("trace urn:test:who {AS_OF}"),
        format!("explain urn:test:who {AS_OF}"),
        format!("why urn:test:who {AS_OF}"),
    ] {
        let world = scoped();
        world.line(&line).unwrap();
        world.assert_every_call_in(AS_OF_IN_CORRIDOR);
    }
}

/// A clock on the host's own corridor gives way to the line's, as the innermost always does.
#[test]
fn an_as_of_clock_replaces_a_clock_on_the_scope() {
    const HOSTS: u64 = 1_000;
    let scope = Scope::empty().with_named_at(
        Iri::parse("urn:ctx:player:alice").unwrap(),
        Arc::new(corridor_space()),
        Arc::new(FixedClock::at(HOSTS)),
    );
    let (engine, calls) = engine_over(kernel_over(Arc::new(root_space())));
    let world = World {
        engine: engine.with_scope(scope),
        calls,
    };
    assert_eq!(
        world.line("source urn:test:now").unwrap(),
        format!("corridor:{HOSTS}")
    );
    assert_eq!(
        world.line(&format!("source urn:test:now {AS_OF}")).unwrap(),
        format!("corridor:{PINNED}")
    );
}

/// Refused lines change nothing: a bad instant leaves the next line in the corridor, and no
/// time doors is still a refusal, never the corridor at the live time.
#[test]
fn a_refused_as_of_line_leaves_the_scope_in_place() {
    let world = scoped();
    let bad = world
        .line("source urn:test:who as-of=yesterday")
        .unwrap_err();
    assert!(bad.contains("is not an instant"), "{bad}");
    assert_eq!(
        world.line("source urn:test:who").unwrap(),
        "corridor:source:"
    );

    let calls: Arc<Mutex<Vec<Call>>> = Arc::default();
    let no_doors = Engine::new(Recording {
        kernel: kernel_over(Arc::new(root_space())),
        calls,
    })
    .with_scope(corridor());
    let refused = output(&no_doors, &format!("source urn:test:who {AS_OF}")).unwrap_err();
    assert!(refused.contains("time doors"), "{refused}");
}

#[test]
fn with_scope_and_with_as_of_doors_compose_in_either_order() {
    let over = || {
        Engine::new(Recording {
            kernel: kernel_over(Arc::new(root_space())),
            calls: Arc::default(),
        })
    };
    let line = format!("source urn:test:who {AS_OF}");
    for engine in [
        over().with_scope(corridor()).with_as_of_doors(doors()),
        over().with_as_of_doors(doors()).with_scope(corridor()),
    ] {
        assert_eq!(output(&engine, &line).unwrap(), "corridor:source:");
    }
}

/// A failing line does not drop the corridor either.
#[test]
fn a_failed_line_leaves_the_scope_in_place() {
    let world = scoped();
    assert!(world.line("source urn:test:nothing-binds-this").is_err());
    world.assert_every_call_in(CORRIDOR);
    assert_eq!(
        world.line("source urn:test:who").unwrap(),
        "corridor:source:"
    );
}

// ---- Without a scope, and where a chain cannot go --------------------------------------

const EVERY_PATH: &[&str] = &[
    "source urn:test:who",
    "source urn:test:who in=x",
    "sink urn:test:who hello",
    "exists urn:test:who",
    "delete urn:test:who",
    "source urn:test:stream | urn:test:stage",
    "source urn:test:stream | urn:test:stage | urn:test:stage",
    "source urn:test:stream | sink urn:test:who",
    "source urn:test:list | ( urn:test:stage ; urn:test:stage )",
    "source urn:test:list .. urn:test:stage",
    "cache urn:test:who",
    "trace urn:test:who",
    "explain urn:test:who",
    "why urn:test:who",
];

/// An engine with no scope issues what it always issued: the empty chain, the root's bindings.
#[test]
fn an_engine_without_a_scope_stays_in_the_root() {
    for line in EVERY_PATH {
        let world = bare();
        world.line(line).unwrap();
        world.assert_every_call_in(ROOT);
    }
    let world = bare();
    assert_eq!(world.line("source urn:test:who").unwrap(), "root:source:");
    assert_eq!(
        world.line("sink urn:test:who hello").unwrap(),
        "root:sink:hello"
    );
    assert!(!world
        .line("trace urn:test:who")
        .unwrap()
        .contains("scope  "));
}

/// `with_scope(Scope::empty())` is no scope at all.
#[test]
fn the_empty_scope_is_no_scope() {
    for line in EVERY_PATH {
        let (engine, calls) = engine_over(kernel_over(Arc::new(root_space())));
        let world = World {
            engine: engine.with_scope(Scope::empty()),
            calls,
        };
        world.line(line).unwrap();
        world.assert_every_call_in(ROOT);
    }
}

/// The sweep under the scope: no call on any path takes a method without a chain.
#[test]
fn every_path_stays_in_the_scope() {
    for line in EVERY_PATH {
        let world = scoped();
        world.line(line).unwrap();
        world.assert_every_call_in(CORRIDOR);
    }
}

/// A resolver that overrides nothing about chains, like a wire resolver: no transport carries
/// spaces and a clock.
struct Wire;
impl Resolver for Wire {
    fn issue(&self, _: Request) -> Result<(Representation, CacheStatus)> {
        Ok((text("answered outside the corridor"), CacheStatus::Miss))
    }
    fn is_cached(&self, _: &Request, _: &Capability) -> bool {
        true
    }
    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        None
    }
}

/// Over a resolver that cannot carry the chain the line is refused, never answered outside it.
#[test]
fn a_resolver_that_cannot_carry_the_scope_refuses_it() {
    let engine = Engine::new(Wire).with_scope(corridor());
    for line in [
        "source urn:test:who",
        "sink urn:test:who hello",
        "delete urn:test:who",
        "exists urn:test:who",
    ] {
        let refused = output(&engine, line).unwrap_err();
        assert!(refused.contains("cannot resolve in the chain"), "{refused}");
    }
    assert_eq!(output(&engine, "cache urn:test:who").unwrap(), "not cached");
    assert_eq!(
        output(&Engine::new(Wire), "source urn:test:who").unwrap(),
        "answered outside the corridor",
        "the same resolver with no scope answers as it always did"
    );
}

/// `list` is the root's catalog: `Resolver::entries` takes no chain, and a host that must show
/// its corridor's names supplies them. Pinned because the doc on `with_scope` says so.
#[test]
fn list_shows_the_root_catalog_not_the_corridors() {
    let world = scoped();
    let listed = world.line("list").unwrap();
    assert!(listed.contains("urn:test:list"), "{listed}");
    assert!(!listed.contains("urn:test:only-here"), "{listed}");
}

/// The async entry point a browser frontend drives holds the same chain.
#[test]
fn eval_async_stays_in_the_scope() {
    let world = scoped();
    let action = block_on(world.engine.eval_async("source urn:test:who"));
    match action {
        Action::Output(entry) => assert_eq!(entry.result.unwrap(), "corridor:source:"),
        _ => panic!("output"),
    }
    world.assert_every_call_in(CORRIDOR);
}
