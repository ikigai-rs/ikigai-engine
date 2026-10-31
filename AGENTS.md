# Working in this repository

`ikigai-engine` holds three ikigai libraries that left the ikigai-cli workspace: `ikigai-resolve`
(the resolver seam), `ikigai-engine` (the REPL engine) and `ikigai-intray` (the intray
tuplespace). These rules are for every agent and every person working here. `CLAUDE.md` is a link
to this file.

- `ikigai-engine` and `ikigai-resolve` compile to `wasm32-unknown-unknown`, and CI checks the
  engine there under its default features. Keep `std::time::Instant::now` and
  `SystemTime::now` out of them (`clippy.toml` bans both); take time from the injected
  `ikigai_core::Clock`.
- The three crates release together at one version (`[workspace.package]`).
- Read `ACCESSIBILITY.md` before any change to output people read: messages, pages, terminal
  text.
- Security reports go through the organization's security policy (GitHub private vulnerability
  reporting), never a public issue.
- Every change lands through a pull request with green CI (`.github/workflows/ci.yml`: fmt,
  clippy with warnings denied, tests and docs across the workspace, plus its other jobs).
- Never invent testing, tool results or conformance evidence, and never weaken a test to make a
  change pass.
- Use American spelling in everything you write: code comments, docs, commit messages and pull
  requests.
