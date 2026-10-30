//! Reproduce ledger #2619: a name capture inside an argument reference.
//!
//! Run with `cargo run -p ikigai-engine --example reference_capture`.
//! This currently fails at the binding parser; it is an executable reproduction,
//! not a claim that reference templates are supported.

use ikigai_engine::names::{parse_binding, render_binding};

fn main() {
    let binding = "urn:river:search:{file} = source urn:sparql:select \
                   graph=urn:file:river.nt query=<urn:file:searches/{file}>";
    let rule = parse_binding(binding).expect("a capture inside a reference should be accepted");
    let rendered = render_binding(&rule).expect("the binding should render losslessly");
    assert_eq!(parse_binding(&rendered).unwrap(), rule);
}
