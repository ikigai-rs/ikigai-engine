# Reference captures: reproduction and dependency blocker (#2619)

The saved-search binding currently fails before its capture can be filled:

```text
urn:river:search:{file} = source urn:sparql:select graph=urn:file:river.nt query=<urn:file:searches/{file}>
```

Run the executable reproduction from the repository root:

```sh
cargo run -p ikigai-engine --example reference_capture
```

It asserts the intended behavior (acceptance and a lossless names-text round trip).
It currently panics because `urn:file:searches/{file}` is not an IRI. This example
is deliberately a manual reproduction, not an automated passing regression test.

## Why a parser change alone cannot fix this

`names::parse_binding` returns `ikigai_core::TopologyRule`. Its fixed arguments use
core's `FixedValue`, whose published variants in 0.2.8 are `Value(String)`,
`Reference(Iri)`, and `Capture(String)`. None represents an IRI template filled
from captures. `Capture` is explicitly a whole inline value; `Reference` already
requires a parsed IRI. The same limitation exists at the workspace's core 0.2.6
API floor. Version 0.2.8 was the latest published core found during reproduction.

`names::fixed_argument` calls `Word::reference`, which parses the bracketed text
as an IRI immediately. Delaying that call would only move the problem: core's
alias table validates and resolves these rules, fills their arguments, exposes
their contracts, and serializes their topology. Engine does not own that runtime.
Percent-encoding the braces at parse time would create a constant reference to a
file literally containing braces rather than a captured reference.

This change records the reproduction; **the bug is not fixed**. An upstream core
release needs a reference-template argument representation with validation,
capture expansion, runtime reference semantics, and lossless topology round trips.
Engine can then raise its core floor, parse and render that representation, and
add automated tests covering:

- The binding above, including names-text and Turtle round trips.
- Resolving `urn:river:search:since.rq` and reading
  `urn:file:searches/since.rq` by reference.
- Captured `/`, `:`, and percent escapes being encoded as capture data, with no
  path or IRI delimiter injection.
- Unknown captures, malformed templates, and quoted literal values.

No runtime behavior or dependency requirement is changed by this reproduction.
