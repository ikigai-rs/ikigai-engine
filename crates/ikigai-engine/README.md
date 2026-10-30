# ikigai-engine

The **renderer-agnostic REPL engine** for [ikigai](https://crates.io/crates/ikigai-core).
`Engine` parses a request line, issues it against a kernel through the
[ikigai-resolve](https://crates.io/crates/ikigai-resolve) `Resolver` seam, and
returns an `Action` describing what to display — knowing nothing about terminals or
rendering. The plain line REPL, the `ratatui` TUI, and the browser frontend all
drive this *same* engine and present its `Action` however suits their medium, so it
was pulled out of the [ikigai-cli](https://crates.io/crates/ikigai-cli) binary into
its own crate for the browser build to reuse unchanged.

## What it does

- **Self-description-driven `source`.** Rather than assuming an `in` argument, the
  engine asks the target endpoint for its parameter contract (a `Meta` request
  rendered as `application/json`) and routes by it — handling an endpoint that reads
  a differently-named argument, several arguments, or only a grammar binding. A
  `key=value` word names a declared argument, once: `graph=A graph=B` is refused as an invalid
  argument `graph`, "given twice", never narrowed to the last value. Positional
  or piped text fills the one argument left unnamed. An unquoted `key=<iri>` sends the
  argument as a reference (`ArgRef::Reference`), which the kernel settles by the target's
  contract, and `plan` renders as `ik:ref <iri>`; a quoted `key="<iri>"` is the literal text.
  The contract is fetched through `issue`, so it works the
  same against a remote kernel. When the fetch FAILS (a kernel with no Meta renderer,
  a refusal, an answer that is not a JSON description), routing falls back to the
  conventional `in`, as it always did, and since 0.1.62 the line says so:
  `Engine::last_contract_notes` names each target and the reason, and the CLI prints
  `[contract unavailable for …]` on stderr. A target that does not resolve has no
  contract to read and is not noted; its own request reports that.
- **A pipeline grammar**, parsed by one recursive-descent parser:

  | connector | meaning |
  |-----------|---------|
  | `a \| b` | pipe — feed each stage's whole output into the next as its input |
  | `a .. b` | map — run `b` once per newline-separated item of `a`'s output, rejoining |
  | `( a ; b )` | fork/join — fan the same input to each branch, join their outputs |
  | `"…"` | quote an operator or whitespace so it's data, not structure |

  For `..`, a final newline terminates the last item; blank items within the list
  are preserved. Empty input has no items.

  `compose` (`$a{<iri>}` transclusion) markers assemble nested resources in one pull.

  A pipe passes its value by value, with one exception: `source <iri> | <stage>`, where
  `<stage>` declares itself an incremental transreptor, hands the stage its upstream by
  reference (`content=<iri>`). The stage reads the upstream's body as it comes and stops
  where it likes, so nothing between the two is collected, and the line resolves as one
  request (one entry in the cache summary).

  Otherwise a stage whose next stage is one request (`| <iri>` or `| sink <iri>`) hands it
  its body as it produced it, ATTACHED as the argument the pipe fills (`content` for a sink,
  the routed argument for a source). A stage that declares that argument `reads_body` reads
  the upstream as it comes, so a one-shot producer is polled only as far as it reads; any
  other stage is handed the same bytes, collected by the kernel under the run's byte budget
  (the trace notes `collected=<argument>`). A value a fork's branches or a `..` map's items
  read, and a line's final answer, are collected once under that budget: `OverBudget` past
  it, never a shorter value. A mounted stage is sent the collected bytes, since a content id
  does not cross a wire.

- **Execution traces.** `trace <iri> [args]` resolves a resource and shows its
  execution tree, including cache status, timing, authority and endpoint notes.
  Default traces omit the `defaulted=` note so adding an optional input does not
  change a pinned transcript. Use `trace --defaults <iri> [args]` to include the
  omitted inputs that the kernel defaulted on each node. The flag names the
  additional information explicitly; it does not change the request or its defaults.

- **Cache-status tracking.** Each resolution reports `computed` / `cached` /
  `uncacheable`; a pipeline summarises its stages (`1 cached · 1 computed`). Pipe
  stages thread their `Provenance` downstream, so a transform is no more cacheable
  than its piped input.
- **Concurrent fan-out.** Fork/map branches run in parallel via `block_on` +
  `join_all` over the resolver's async `issue_*` methods (on
  [ikigai-scheduler](https://crates.io/crates/ikigai-scheduler) when one is
  injected); with no spawner the path is a sequential fallback, so the engine still
  compiles to wasm.

With the `plan-reader` feature, `plan_space::space()` binds `urn:plan:eval`,
`urn:plan:validate` and `urn:plan:requires`, and names itself
`urn:iki:space:engine:plan` (`plan_space::SPACE_ID`).

`bind <name> = <verb> <target> [key=value …]` and `unbind <name>` write a named request into
the host's names atom (`Engine::with_names_atom`); a host that names none refuses them. A
binding is checked against its target's contract when it is written (`bind urn:x = source
urn:t:upper nosuch=1` is refused naming `nosuch`), a `{var}` in a template name fills the
target IRI or a whole argument value, and a `urn:plan:eval` binding copies the plan's
`ik:input`s onto the name. A pipeline on the right (`bind urn:q:loud = source urn:file:x |
urn:iki:fn:toUpper`) is compiled to the plan `plan` renders and bound as `source urn:plan:eval`
with that plan inline, so the name is a live composite that recomputes when what it reads is
cut, not a `sink` snapshot (`plan-reader`); every fork the text face makes binds this way,
`.. ( … )` included, and a pipeline `plan` cannot render is refused with the renderer's
message. The atom holds the names text form (`names`, media type
`text/x-ikigai-names`, one binding per line). With `plan-reader`, `names::space()` binds the
lossless transreptor pair `urn:names:to-rdf` / `urn:names:from-rdf` between that form and the
`ik:Alias` Turtle core builds, and names itself `urn:iki:space:engine:names`.

A plan spells a fork every way the text face feeds one (vocab 0.2.8, `ik:Stage`): over a
whole upstream (`ik:upstream`), over each item of one (`a .. ( b ; c )`, `ik:mapOver` on the
fork), as a fork's branch (`ik:forkOf`), and binding its join to a name (`ik:binds`). In a
mapped fork each item is its own request for every branch, with its own cache entry and
thread, so changing one item of the list recomputes only that item's branches; the per-item
joins are rejoined with newlines in list order, and a failing item aborts the run, as `..`
does in the text face.

A stored plan run by `execute` pipes as the text face does: a step whose only reader is one
`ik:pipeFrom` edge hands that step its body attached, and a value read more than once (a
fork's branches, a `..` map's items, a step referencing its `ik:binds`) or the plan's result
is collected once under the run's byte budget. `urn:plan:eval` still hands every step its
input by value: a sub-request issued from an invocation cannot carry an attached body.

`execute <plan> key=value` passes `key=value` to the plan's declared parameter, as
`urn:plan:eval` does: a `key=value` word on the spec's last stage that its target does not take
as an argument is a parameter, and one the plan does not declare is refused by name.

A line whose first non-blank character is `#` is a comment (so a `#!` shebang is one too); a
`#` anywhere else keeps its meaning. `ikigai_engine::script` and `Engine::run_script` run a
script of REPL lines (a `.iki` file) in file order: `# @param <name>` or `# @param <name> =
<default>` declares a parameter a line uses as `{name}`, an undeclared `{x}` or parameter is
refused before any line runs, and the run stops at the first failing line, reporting its
number and error. A script does not compile to a plan yet: the process vocabulary has no term
for ordered, independent steps (`ik:step` is unordered, and the runner runs only what the one
`ik:result` depends on).

`config` is a small user-settings reader (used by the `config` command and the TUI's
keybindings); on a target with no config directory it reports defaults.

## License

MIT OR Apache-2.0.
