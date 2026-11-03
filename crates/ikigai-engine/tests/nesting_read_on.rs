//! Does anything in the engine read on past a parse error, after the nesting scan admitted a
//! document (ledger #2953)? `ikigai_rdf::check_turtle_nesting` reads a `'` as opening a string that
//! crosses lines. oxttl's N-Triples parser errors at that `'` and RESUMES at the next line, so the
//! document below hides a deep triple term from the scan, and a reader in that mode survives only
//! by stopping at its first error. (The document is `ikigai-rdf`'s `tests/nesting_reconcile.rs`.)
//! Every reader here parses Turtle, whose lexer, on the line break, consumes on to the closing `'`
//! as the scan does, so the term is inside the bad string and nothing hides.
//!
//! Every probe runs in a CHILD PROCESS on a 2 MiB thread, the harness of `plan_nesting.rs`.
#![cfg(feature = "plan-reader")]

use std::process::Command;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Exact, FnEndpoint, Invocation, Iri, Kernel,
    MetaRenderer, ReprType, Representation, Request, Verb,
};
use ikigai_engine::{Action, Engine};

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

const PROBE: &str = "IKIGAI_ENGINE_NESTING_READ_ON_PROBE";
/// Far past where the parser's recursion dies on a 2 MiB thread, as in `nesting_reconcile.rs`.
const LETHAL: usize = 30_000;

fn nested(levels: usize) -> String {
    format!(
        "{}<urn:ex:o>{}",
        "<<( <urn:ex:s> <urn:ex:p> ".repeat(levels),
        " )>>".repeat(levels)
    )
}

/// An unterminated `'`, the deep term on the NEXT line, and the `'` that closes the scan's string.
fn hidden(graph: &str, levels: usize) -> String {
    let graph = if graph == "nq" { " <urn:ex:g>" } else { "" };
    format!(
        "<urn:ex:s> <urn:ex:q> 'x{graph} .\n<urn:ex:s> <urn:ex:p> {}{graph} . # '\n",
        nested(levels)
    )
}

fn door(name: &str, text: String) -> String {
    let kernel = Kernel::new(Arc::new(ikigai_core::Fallback::new(vec![
        Arc::new(ikigai_shacl::space()) as Arc<dyn ikigai_core::Space>,
        Arc::new(ikigai_engine::plan_space::space()),
    ])));
    let request = Request::new(
        Verb::Source,
        Iri::parse(format!("urn:plan:{name}")).expect("IRI"),
    )
    .with_arg("in", ArgRef::Inline(text.into_bytes()));
    match block_on(kernel.issue(request, &Capability::root())) {
        Ok(r) => format!("ok {}", String::from_utf8_lossy(&r.bytes)),
        Err(e) => format!("err {e}"),
    }
}

/// The REPL over a store holding `text`, where `execute` and `bind` read it through `Plan::read`.
fn repl(line: &str, text: String) -> String {
    let stored = FnEndpoint::new("plan-store", move |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new("text/turtle"),
            text.clone().into_bytes(),
        ))
    })
    .with_description(Description::new("plan-store").verb(Verb::Source));
    let atom = FnEndpoint::new("atom", |_: &Invocation<'_>| {
        Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
    })
    .with_description(Description::new("atom").verb(Verb::Source).verb(Verb::Sink));
    let root = ikigai_core::Fallback::new(vec![
        Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:t:hidden"), stored)
                .bind(Exact::new("urn:t:names"), atom),
        ) as Arc<dyn ikigai_core::Space>,
        Arc::new(ikigai_engine::plan_space::space()),
    ]);
    let engine = Engine::new(Kernel::with_meta_renderer(
        Arc::new(root),
        Arc::new(JsonRenderer),
    ))
    .with_names_atom(Iri::parse("urn:t:names").expect("IRI"));
    match engine.eval(line) {
        Action::Output(entry) => format!("{:?}", entry.result),
        _ => panic!("`{line}` should produce output"),
    }
}

/// How many triples a reader that skips errors gets, and how many errors it skipped.
fn read_on<E>(triples: impl Iterator<Item = Result<oxrdf::Triple, E>>) -> String {
    let (mut good, mut bad) = (0, 0);
    for triple in triples {
        match triple {
            Ok(_) => good += 1,
            Err(_) => bad += 1,
        }
    }
    format!("read on: {good} triples, {bad} errors")
}

fn run(path: &str, graph: &str, levels: usize) -> String {
    let text = hidden(graph, levels);
    let bytes = text.as_bytes();
    let scan = ikigai_store::depth::check_rdf_nesting(bytes, oxigraph::io::RdfFormat::Turtle, "in")
        .is_ok();
    let verdict = match path {
        "validate" | "eval" | "requires" => door(path, text.clone()),
        "execute" => repl("execute urn:t:hidden", text.clone()),
        "bind" => repl(
            "bind urn:x = source urn:plan:eval in=<urn:t:hidden>",
            text.clone(),
        ),
        "read-on-turtle" => read_on(oxttl::TurtleParser::new().for_slice(bytes)),
        "read-on-ntriples" => read_on(oxttl::NTriplesParser::new().for_slice(bytes)),
        other => panic!("no path named {other}"),
    };
    format!("scan admitted={scan}; {}", verdict.replace('\n', " "))
}

/// The child's half: inert unless a parent named a probe.
#[test]
fn probe_child() {
    let Ok(spec) = std::env::var(PROBE) else {
        return;
    };
    let parts: Vec<String> = spec.split(':').map(str::to_string).collect();
    let (path, graph) = (parts[0].clone(), parts[1].clone());
    let levels: usize = parts[2].parse().expect("levels");
    let outcome = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || run(&path, &graph, levels))
        .expect("spawn")
        .join()
        .expect("join");
    println!("\nOUTCOME {outcome}");
    std::process::exit(0);
}

/// The parent's half: what the child said, or `Err` naming how it died.
fn try_probe(path: &str, graph: &str, levels: usize) -> Result<String, String> {
    let out = Command::new(std::env::current_exe().expect("exe"))
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, format!("{path}:{graph}:{levels}"))
        .output()
        .expect("child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{path} did not survive a 2 MiB thread: {} - {}",
            out.status,
            stderr
                .lines()
                .find(|l| l.contains("overflow"))
                .unwrap_or(&stderr)
        ));
    }
    let at = stdout
        .find("\nOUTCOME ")
        .ok_or_else(|| format!("{path} reported nothing: {stdout}"))?;
    Ok(stdout[at + "\nOUTCOME ".len()..]
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

/// Every path that scans and then parses: the scan admits the document, the parser's first error
/// is the answer, and the child lives.
#[test]
fn every_path_that_scans_answers_the_first_error() {
    for path in ["validate", "eval", "requires", "execute", "bind"] {
        for graph in ["nt", "nq"] {
            let outcome = try_probe(path, graph, LETHAL).unwrap_or_else(|died| panic!("{died}"));
            println!("{path} [{graph}]: {outcome}");
            assert!(outcome.starts_with("scan admitted=true; "), "{outcome}");
            assert!(outcome.contains("not valid Turtle"), "{path}: {outcome}");
        }
    }
}

/// The control: the same document, read on past the error in oxttl's N-Triples mode, aborts. A
/// shallow one shows why: the hidden statement really is built. In Turtle mode the term is inside
/// the bad string, so reading on builds nothing even at this depth.
#[test]
fn reading_on_past_the_error_builds_the_hidden_term_and_aborts() {
    let shallow = try_probe("read-on-ntriples", "nt", 3).unwrap_or_else(|died| panic!("{died}"));
    println!("read-on-ntriples at 3: {shallow}");
    assert!(shallow.contains("read on: 1 triples"), "{shallow}");
    let died = try_probe("read-on-ntriples", "nt", LETHAL);
    println!("read-on-ntriples at {LETHAL}: {died:?}");
    assert!(died.is_err(), "expected an abort, got {died:?}");

    let turtle = try_probe("read-on-turtle", "nt", LETHAL).unwrap_or_else(|died| panic!("{died}"));
    println!("read-on-turtle at {LETHAL}: {turtle}");
    assert!(turtle.contains("read on: 0 triples"), "{turtle}");
}
