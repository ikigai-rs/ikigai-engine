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
| Last reviewed | 2026-11-13 |

## What it emits that people read

- **The REPL's text** (`crates/ikigai-engine`): `HELP`, the command list a host shows for
  `help` (`src/engine.rs`); each evaluated line as an `Action::Output` entry whose result is the
  answer or an error string; the cache verdict of a line as a word (`CacheStats::label`); the
  `explain`, `why` and `dependents` views (`src/engine/why.rs`), which pass core's own text
  through; the refusal of `login` and `logout` in a sealed session (`src/engine.rs`); `bind` and names-file messages (`src/engine/bind.rs`, `src/names.rs`); script and
  plan refusals (`src/script.rs`, `src/plan.rs`); and a near-name suggestion under an unresolved
  name (`src/suggest.rs`).
- **One stderr line** from the engine, when a configured keybinding is not supported
  (`src/config.rs`, starting `ikigai:`).
- **Resolver refusals** (`crates/ikigai-resolve/src/lib.rs`, `home.rs`): error text returned to
  the host.
- **Intray diagnostics** (`crates/ikigai-intray/src/lib.rs`): lines on stderr, each starting
  `ikigai-intray:`, when a reactor loses its lease or stops watching, a note cannot be written,
  or an interrupted tuple is recovered; and the errors its verbs return.

Documentation is also read on GitHub or in an editor: the README, this statement, contributor
instructions and Rust API documentation. No current rendering evaluation of those surfaces
was performed.

Not this repository's: the screen, prompt, colors and `error:` prefix a host puts around this
text (the `ikigai` binary's REPLs are ikigai-cli's), and the SVG a `show` command passes on, which
the resource that drew it owns.

## The rules that output follows

These are the output rules to evaluate. The table below records their verification scope;
they are not a claim that every possible resource answer follows them:

- Engine-authored help, errors and diagnostics should communicate in plain text without
  depending on color, terminal escapes or markup. The host decides how to style text; arbitrary
  resource answers and forwarded core views need their own evaluation.
- A cache verdict is a word (`cached`, `computed`, `uncacheable`), or a count of each joined by
  ` · ` for a line that issued several requests.
- An error names what it is about: an unknown command by the word typed (and points at `help`);
  a missing or invalid argument by its name, followed by that argument's own summary, the values
  it accepts when it is an enumeration, and, when a piped value would have filled it, `pipe a
  value in or pass <name>=…`; an ambiguous value by the target and its arguments; a parse error
  by the token, with a hint when the line used `'…'` quotes the engine does not honor.
- Under an unresolved name, a near name is suggested only when one is actually bound, and a host
  that named its build says what it is and gives upgrade instructions conditional on a newer
  release being available. It does not check release availability or claim that another
  release binds the name.
- Every command the engine dispatches is listed in `HELP`.

## How it is tested

Every named check has a result: Pass, Fail or Blocked, with evidence, following the
[closing-sweep format](https://github.com/ikigai-rs/ikigai-gonk-starter/pull/91).
A Pass covers only the stated check, not overall accessibility or WCAG conformance.
The current test run below used main at `2340b8d7701a5670f7cfe85cdee23dc23c114118`,
on 2026-11-13, before this documentation change, with Cargo 1.99.0 and rustc 1.99.0
(`b940084d7`). The command was `cargo test --workspace --all-features`; CI repeats it
through [the workspace workflow](.github/workflows/ci.yml). These Rust tests check text and
behavior, not how a host renders or announces the output.

| Check | Result | Evidence |
|---|---|---|
| Missing, invalid and piped argument help | Pass | `crates/ikigai-engine/src/engine.rs`: `a_missing_pipe_target_names_its_summary_and_how_to_give_it`, `a_missing_argument_that_is_not_the_pipe_target_gets_its_summary_only`, `an_invalid_argument_names_its_summary_and_accepted_values`, `a_sink_missing_its_content_names_the_pipe` and `a_fanned_out_branch_gets_the_argument_help_too`, run by the command above. Pins the tested argument summaries, accepted values and pipe guidance; not every error path. |
| Unresolved-name suggestions and conditional upgrade wording | Pass | `crates/ikigai-engine/src/engine.rs`: `an_unresolved_name_names_the_near_binding_that_is_bound` and `an_unresolved_name_names_the_host_build_and_its_upgrade`; unit tests in `crates/ikigai-engine/src/suggest.rs`, run by the command above. Checks bound candidates and wording, not release availability or spoken pronunciation. |
| Cache verdict words | Pass | `crates/ikigai-engine/src/engine.rs`: `cache_label_formats_single_and_mixed_outcomes` and `non_resolving_commands_have_no_cache_label`, run by the command above. Pins single and mixed labels, not host styling. |
| Sealed-session login and logout refusal | Pass | `crates/ikigai-engine/src/engine.rs`: `a_sealed_session_refuses_login_and_logout_and_keeps_its_capability`, run by the command above. Checks the refusal and retained capability, not its announcement by a host. |
| Script failure line and recursive-script refusal | Pass | `crates/ikigai-engine/tests/script.rs`: `a_failing_line_of_a_stored_script_stops_both_and_names_its_line` and `a_script_that_runs_itself_is_stopped`, run by the command above. Pins the script, line and recursion chain in these cases. |
| Command coverage in HELP | Pass | `crates/ikigai-engine/src/engine/why.rs`: `every_command_is_in_help_and_dispatched`, run by the command above. Checks help against dispatch, not reading order or layout. |
| Repository-wide plain text, no color, terminal escapes or markup | Blocked | Historical source review on 2026-10-31; CI does not repeat that review. No current exhaustive audit or escape scan was run. Resource answers and core views pass through the engine; the tests above cannot establish this for arbitrary output. |
| Unknown-command, ambiguous-value and parse-error wording, including single-quote hint | Blocked | Historical source review on 2026-10-31; CI does not repeat that review. The current run is not an exhaustive wording evaluation of these paths. |
| Intray recovery outcome in human-readable words | Fail | Source inspection on 2026-11-13: `crates/ikigai-intray/src/lib.rs`, `Reactor::try_watch`, formats `r.outcome` with `{:?}` in the recovery diagnostic. This verifies debug formatting, not its effect with assistive technology; no WCAG failure is inferred. |
| Backtick pronunciation, HELP continuation context and error-prefix indentation | Blocked | Historical source review on 2026-10-31 identified the limitations below; CI does not repeat a usability evaluation. No current host or assistive-technology session evaluated their effect. |
| Browser rendering, contrast, zoom and reflow | Blocked | No browser evaluation was run; this repository supplies libraries, and no host page was selected for this review. Host presentation still needs evaluation. |
| Documentation rendering and reading order | Blocked | No current GitHub, editor or rendered Rust API documentation evaluation was run; text tests do not evaluate those surfaces. |
| Manual keyboard | Blocked | No person performed a keyboard pass in a host; no interactive host session was available for this review. |
| Automated accessibility checker | Blocked | No axe or other accessibility checker was run; no rendered host page or checker was configured. Rust tests are not an accessibility checker. |
| Screen reader | Blocked | No screen reader was used on this output; no screen reader session was available for this review. |
| Screen magnifier | Blocked | No magnifier was used on this output; no magnifier session was available for this review. |
| Speech input | Blocked | No speech-input software was used on this output; no speech-input session was available for this review. |
| Switch access | Blocked | No switch-access software was used on this output; no switch-access session was available for this review. |
| Braille display | Blocked | No braille display was used on this output; no braille display was available for this review. |

## Known limitations

Source observations from 2026-10-31, not assistive-technology testing; the recovery
format was rechecked on 2026-11-13. The table above records the remaining evaluation gaps:

- The intray's recovery line prints the outcome in Rust's debug form (`{:?}`), not in words.
- Messages wrap names in backticks (`` `name` ``), Markdown punctuation that a screen reader may
  read aloud in a terminal.
- `HELP` is laid out as two columns padded with spaces, with long descriptions continued on
  indented lines; read line by line, a continuation does not repeat the command it belongs to.
- An error string's continuation lines are indented with spaces to sit under a host's `error: `
  prefix, so their layout assumes a host that writes that prefix.

## Contributing

Read this file before changing any output listed above: `HELP`, an error or refusal, a view, a
diagnostic line. Keep meaning in words and name the input a message is about. In the pull
request, give each accessibility check a `Check | Result | Evidence` row: Pass, Fail or
Blocked, with its command or test, tool and version, or why it could not run. Keep unrun
checks Blocked, including assistive technology nobody used; a historical review CI does not
repeat is Blocked with its date.

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
| 2026-11-06 | Made unresolved-name upgrade advice conditional on a newer release being available; pinned the wording in an engine unit test. |
| 2026-11-13 | Recorded current text-test evidence and explicit Pass, Fail and Blocked results; retained historical review and host/assistive-technology gaps. |
