# Accessibility

What `ikigai-engine`, `ikigai-resolve` and `ikigai-intray` emit that people read, the rules that
output follows, how it is tested, and how to report a barrier. They are libraries: a host (a
REPL, an editor, a browser page) decides how the text reaches a person. It is not a conformance
claim. Adapted from the [ACCESSIBILITY.md
template](https://github.com/mgifford/ACCESSIBILITY.md) (MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA, for any page that presents this output; this repository writes text only |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-engine/issues/new?labels=accessibility) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-engine/security/advisories/new) |
| Last reviewed | 2026-10-31 |

## What it emits that people read

- **The REPL's text** (`crates/ikigai-engine`): `HELP`, the command list a host shows for
  `help` (`src/engine.rs`); each evaluated line as an `Action::Output` entry whose result is the
  answer or an error string; the cache verdict of a line as a word (`CacheStats::label`); the
  `explain`, `why` and `dependents` views (`src/engine/why.rs`), which pass core's own text
  through; `bind` and names-file messages (`src/engine/bind.rs`, `src/names.rs`); script and
  plan refusals (`src/script.rs`, `src/plan.rs`); and a near-name suggestion under an unresolved
  name (`src/suggest.rs`).
- **One stderr line** from the engine, when a configured keybinding is not supported
  (`src/config.rs`, starting `ikigai:`).
- **Resolver refusals** (`crates/ikigai-resolve/src/lib.rs`, `home.rs`): error text returned to
  the host.
- **Intray diagnostics** (`crates/ikigai-intray/src/lib.rs`): lines on stderr, each starting
  `ikigai-intray:`, when a reactor loses its lease or stops watching, a note cannot be written,
  or an interrupted tuple is recovered; and the errors its verbs return.

Not this repository's: the screen, prompt, colors and `error:` prefix a host puts around this
text (the `ikigai` binary's REPLs are ikigai-cli's), and the SVG a `show` command passes on, which
the resource that drew it owns.

## The rules that output follows

Verified by reading the source on 2026-10-31:

- No color, terminal escape or markup is written: every answer, error and diagnostic is plain
  text, and the host decides how to style it.
- A cache verdict is a word (`cached`, `computed`, `uncacheable`), or a count of each joined by
  ` · ` for a line that issued several requests.
- An error names what it is about: an unknown command by the word typed (and points at `help`);
  a missing or invalid argument by its name, followed by that argument's own summary, the values
  it accepts when it is an enumeration, and, when a piped value would have filled it, `pipe a
  value in or pass <name>=…`; an ambiguous value by the target and its arguments; a parse error
  by the token, with a hint when the line used `'…'` quotes the engine does not honor.
- Under an unresolved name, a near name is suggested only when one is actually bound, and a host
  that named its build says what it is and how to upgrade.
- Every command the engine dispatches is listed in `HELP`.

## How it is tested

- `crates/ikigai-engine/src/engine.rs`:
  `a_missing_pipe_target_names_its_summary_and_how_to_give_it`,
  `a_missing_argument_that_is_not_the_pipe_target_gets_its_summary_only`,
  `an_invalid_argument_names_its_summary_and_accepted_values`,
  `a_sink_missing_its_content_names_the_pipe` and `a_fanned_out_branch_gets_the_argument_help_too`
  pin the argument help; `an_unresolved_name_names_the_near_binding_that_is_bound` and
  `an_unresolved_name_names_the_host_build_and_its_upgrade` pin the lines under an unresolved
  name; `cache_label_formats_single_and_mixed_outcomes` and
  `non_resolving_commands_have_no_cache_label` pin the verdict words.
- `crates/ikigai-engine/src/suggest.rs`'s unit tests pin when a suggestion is made and its
  wording; `src/engine/why.rs`'s `every_command_is_in_help_and_dispatched` holds `HELP` against
  the dispatcher.
- **No automated accessibility checker, browser or assistive technology has been used on this
  output.**

## Known limitations

Found by reading the source on 2026-10-31, not by testing:

- The line under an unresolved name says `a name newer than this build needs an upgrade`, even
  when the name is a typo that no release binds.
- The intray's recovery line prints the outcome in Rust's debug form (`{:?}`), not in words.
- Messages wrap names in backticks (`` `name` ``), Markdown punctuation that a screen reader may
  read aloud in a terminal.
- `HELP` is laid out as two columns padded with spaces, with long descriptions continued on
  indented lines; read line by line, a continuation does not repeat the command it belongs to.
- An error string's continuation lines are indented with spaces to sit under a host's `error: `
  prefix, so their layout assumes a host that writes that prefix.

## Contributing

Read this file before changing any output listed above: `HELP`, an error or refusal, a view, a
diagnostic line. Keep meaning in words, name the input a message is about, and say in the pull
request what was tested and what was not.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-engine/issues/new?labels=accessibility) with
the `accessibility` label (the link sets it when GitHub lets you add labels; otherwise a
maintainer adds it): what you were doing, what happened, and your terminal or editor and any
assistive technology, if you are comfortable sharing them. Anything security-sensitive goes
through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-engine/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.

## Change history

| Date | Change |
|---|---|
| 2026-10-31 | First version. |
