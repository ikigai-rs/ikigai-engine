//! A terminal policy must reuse routing Meta and leave upstream values alone.
use std::sync::{Arc, Mutex};

use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, EndpointSpace, Exact, FnEndpoint, Invocation,
    Kernel, MetaRenderer, ReprType, Representation, Request, SpaceEntry, Verb,
};
use ikigai_engine::engine::{Action, Engine};
use ikigai_resolve::{CacheStatus, Resolver};

type Calls = Arc<Mutex<Vec<Request>>>;

struct Json;
impl MetaRenderer for Json {
    fn render(&self, description: &Description, _: &ReprType) -> ikigai_core::Result<Representation> {
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

    async fn issue_as_async(&self, mut request: Request, capability: &Capability) -> ikigai_core::Result<(Representation, CacheStatus)> {
        // The old adapter has no contract or output role, so it probes again and
        // applies its face rule to every source request it sees.
        if request.verb == Verb::Source {
            let meta = Request::new(Verb::Meta, request.target.clone())
                .with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
            self.calls.lock().unwrap().push(meta.clone());
            self.kernel.issue_as_async(meta, capability).await?;
            if !request.args.contains_key("as") {
                request = request.with_arg("as", ArgRef::Inline(b"text/plain".to_vec()));
            }
        }
        self.calls.lock().unwrap().push(request.clone());
        self.kernel.issue_as_async(request, capability).await
    }

    fn is_cached(&self, _: &Request, _: &Capability) -> bool { false }
    fn entries(&self) -> Option<Vec<SpaceEntry>> { None }
}

fn fixture() -> (Engine, Calls) {
    let producer = FnEndpoint::new("producer", |inv: &Invocation<'_>| {
        let face = inv.inline_str("as").unwrap_or("text/html");
        Ok(Representation::new(ReprType::new("text/plain"), face.as_bytes().to_vec()))
    }).with_description(Description::new("producer").input(ArgSpec::new("in").optional()));
    let consumer = FnEndpoint::new("consumer", |inv: &Invocation<'_>| {
        Ok(Representation::new(ReprType::new("text/plain"), inv.inline_arg("in")?.to_vec()))
    }).with_description(Description::new("consumer").input(ArgSpec::new("in")));
    let space = EndpointSpace::new()
        .bind(Exact::new("urn:test:producer"), producer)
        .bind(Exact::new("urn:test:consumer"), consumer);
    let calls = Arc::new(Mutex::new(Vec::new()));
    (Engine::new(Terminal {
        kernel: Kernel::with_meta_renderer(Arc::new(space), Arc::new(Json)),
        calls: calls.clone(),
    }), calls)
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
    assert_eq!(output(&engine, "source urn:test:producer input"), "text/plain");
    let calls = calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|r| r.verb == Verb::Meta).count(), 1);
}

#[test]
fn terminal_policy_does_not_change_an_intermediate_face() {
    let (engine, _) = fixture();
    assert_eq!(output(&engine, "source urn:test:producer input | urn:test:consumer"), "text/html");
}
