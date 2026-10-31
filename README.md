# ikigai-engine

Three library crates of [ikigai](https://github.com/ikigai-rs), the resource-oriented
computing kernel in Rust: the pieces a host uses to turn a request line into resolutions
against a kernel, and the intray tuplespace.

| crate | what it is |
|-------|------------|
| [`ikigai-resolve`](crates/ikigai-resolve) | The `Resolver` seam: issue a request against a kernel, local or remote, and report how the cache served it. Also the shared file-name and `~` rules a request-derived path goes through. |
| [`ikigai-engine`](crates/ikigai-engine) | The renderer-agnostic REPL engine: parses a request line (pipes, map, fork/join, `compose`), routes arguments by each endpoint's own contract, drives a `Resolver`, and returns an `Action` to display. Compiles to `wasm32-unknown-unknown`. With `plan-reader`, it also reads and runs stored plans (`urn:plan:*`). |
| [`ikigai-intray`](crates/ikigai-intray) | The intray as a tuplespace, `urn:space:{name}`: Sink drops a tuple, Source reads and matches (SPARQL ASK), Delete takes one atomically, and a reactor fires a handler on each drop. Native only. |

`ikigai-engine` depends on `ikigai-resolve`, and `ikigai-intray` depends on `ikigai-resolve`;
the three release together, at one version.

## Why they live here and not in ikigai-cli

They were members of the [ikigai-cli](https://github.com/ikigai-rs/ikigai-cli) workspace and
released in lockstep with the `ikigai` binary. The cli links modules whose own tests depend on
these crates, so their repos formed a cycle no publish order could get through; in a repository
of their own (ledger #2316) they release first, and the modules and the cli follow.

The history came with them: `git log` here reaches back to the engine's first commit in
ikigai-cli (June 2026), and a commit subject's `(ikigai-rs/ikigai-cli#N)` names the cli pull
request it merged in.

## Building

```sh
cargo test --workspace --all-features
cargo clippy -p ikigai-engine --lib --target wasm32-unknown-unknown   # the wasm face
```

`plan-reader` is off by default: it pulls a Turtle stack that does not build for
`wasm32-unknown-unknown`. A native host that executes stored plans turns it on.

## Accessibility

What these crates emit that people read (the REPL's help, answers and errors, the intray's
diagnostics), the rules that text follows and how to report a barrier are in
[ACCESSIBILITY.md](ACCESSIBILITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
