//! Does a `match=` read on past a parse error, after the nesting scan admitted a tuple (ledger
//! #2953)? `ikigai_rdf::check_turtle_nesting` reads a `'` as opening a string that crosses lines.
//! oxttl's N-Triples parser errors at that `'` and RESUMES at the next line, so the tuple below
//! hides a deep triple term from the scan, and a reader in that mode survives only by stopping at
//! its first error. (The document is `ikigai-rdf`'s `tests/nesting_reconcile.rs`.) A match parses
//! Turtle, whose lexer, on the line break, consumes on to the closing `'` as the scan does, so the
//! term is inside the bad string and nothing hides.
//!
//! Every probe runs in a CHILD PROCESS on a 2 MiB thread, the harness of `tuple_nesting.rs`.

mod common;

use common::{drop_tuple, iri, scratch};
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Kernel, Request, Verb};
use ikigai_intray::{space, tuple_id, CAP_OUT, CAP_READ, CAP_TAKE};
use oxigraph::io::{RdfFormat, RdfParser};
use std::process::Command;
use std::sync::Arc;

const PROBE: &str = "IKIGAI_INTRAY_NESTING_READ_ON_PROBE";
/// Far past where the parser's recursion dies on a 2 MiB thread, as in `nesting_reconcile.rs`.
const LETHAL: usize = 30_000;
/// The tuple every probe drops beside the hidden one, and the only one that can match.
const SHALLOW: &[u8] = b"<urn:a> <urn:b> \"c\" .";

/// An unterminated `'`, the deep term on the NEXT line, and the `'` that closes the scan's string.
fn hidden(levels: usize) -> Vec<u8> {
    format!(
        "<urn:ex:s> <urn:ex:q> 'x .\n<urn:ex:s> <urn:ex:p> {}<urn:ex:o>{} . # '\n",
        "<<( <urn:ex:s> <urn:ex:p> ".repeat(levels),
        " )>>".repeat(levels)
    )
    .into_bytes()
}

/// A space holding the shallow tuple and the hidden one, both dropped through `out`, or the hidden
/// one PLANTED on disk.
fn seeded(sub: &str, planted: bool, levels: usize) -> Kernel {
    let root = scratch(sub);
    let k = Kernel::new(Arc::new(space(root.clone())));
    let out = Capability::scoped([CAP_OUT]);
    drop_tuple(&k, &out, "urn:space:q", SHALLOW).expect("drop");
    let deep = hidden(levels);
    if planted {
        std::fs::write(
            root.join("q")
                .join("inbox")
                .join(format!("{}.tuple", tuple_id(&deep))),
            &deep,
        )
        .expect("plant");
    } else {
        drop_tuple(&k, &out, "urn:space:q", &deep).expect("out admits what the scan admits");
    }
    k
}

/// One `match=` at `door` that every RDF tuple satisfies.
fn matched(door: &str, planted: bool, levels: usize) -> String {
    let k = seeded(&format!("read-on-{door}-{planted}"), planted, levels);
    let (verb, cap) = match door {
        "rd" => (Verb::Source, CAP_READ),
        "take" => (Verb::Delete, CAP_TAKE),
        other => panic!("unknown door {other}"),
    };
    match block_on(
        k.issue(
            Request::new(verb, iri("urn:space:q"))
                .with_arg("match", ArgRef::Inline(b"ASK { ?s ?p ?o }".to_vec())),
            &Capability::scoped([cap]),
        ),
    ) {
        Ok(r) => format!("ok {}", String::from_utf8_lossy(&r.bytes).trim()),
        Err(e) => format!("err {e}"),
    }
}

/// The control: oxigraph's own Turtle parser, read past its errors.
fn read_on(format: RdfFormat, levels: usize) -> String {
    let bytes = hidden(levels);
    let (mut good, mut bad) = (0, 0);
    for quad in RdfParser::from_format(format).for_slice(&bytes) {
        match quad {
            Ok(_) => good += 1,
            Err(_) => bad += 1,
        }
    }
    format!("read on: {good} quads, {bad} errors")
}

fn run(path: &str, levels: usize) -> String {
    let scan =
        ikigai_rdf::check_turtle_nesting(&String::from_utf8_lossy(&hidden(levels)), "content")
            .is_ok();
    let verdict = match path {
        "rd-out" => matched("rd", false, levels),
        "take-out" => matched("take", false, levels),
        "rd-planted" => matched("rd", true, levels),
        "take-planted" => matched("take", true, levels),
        "read-on-turtle" => read_on(RdfFormat::Turtle, levels),
        "read-on-ntriples" => read_on(RdfFormat::NTriples, levels),
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
    let (path, levels) = spec.split_once(':').expect("path:levels");
    let (path, levels) = (path.to_string(), levels.parse::<usize>().expect("levels"));
    let outcome = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || run(&path, levels))
        .expect("spawn")
        .join()
        .expect("join");
    println!("\nOUTCOME {outcome}");
    std::process::exit(0);
}

/// The parent's half: what the child said, or `Err` naming how it died.
fn try_probe(path: &str, levels: usize) -> Result<String, String> {
    let out = Command::new(std::env::current_exe().expect("exe"))
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, format!("{path}:{levels}"))
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

/// `out` admits the tuple, and a match over it, dropped or planted, survives: the tuple is not RDF
/// to a match, and the shallow one still matches.
#[test]
fn a_match_over_the_tuple_survives_and_skips_it() {
    let id = tuple_id(SHALLOW);
    let content = String::from_utf8_lossy(SHALLOW).into_owned();
    for (path, expected) in [
        ("rd-out", &id),
        ("rd-planted", &id),
        ("take-out", &content),
        ("take-planted", &content),
    ] {
        let outcome = try_probe(path, LETHAL).unwrap_or_else(|died| panic!("{died}"));
        println!("{path}: {outcome}");
        assert_eq!(
            outcome,
            format!("scan admitted=true; ok {expected}"),
            "{path}"
        );
    }
}

/// The control: the same tuple, read on past the error in oxttl's N-Triples mode, aborts. A shallow
/// one shows why: the hidden statement really is built. In Turtle mode the term is inside the bad
/// string, so reading on builds nothing even at this depth.
#[test]
fn reading_on_past_the_error_builds_the_hidden_term_and_aborts() {
    let shallow = try_probe("read-on-ntriples", 3).unwrap_or_else(|died| panic!("{died}"));
    println!("read-on-ntriples at 3: {shallow}");
    assert!(shallow.contains("read on: 1 quads"), "{shallow}");
    let died = try_probe("read-on-ntriples", LETHAL);
    println!("read-on-ntriples at {LETHAL}: {died:?}");
    assert!(died.is_err(), "expected an abort, got {died:?}");

    let turtle = try_probe("read-on-turtle", LETHAL).unwrap_or_else(|died| panic!("{died}"));
    println!("read-on-turtle at {LETHAL}: {turtle}");
    assert!(turtle.contains("read on: 0 quads"), "{turtle}");
}
