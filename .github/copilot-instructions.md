See AGENTS.md in project root. # GitHub Copilot Instructions

This project uses AGENTS.md for all coding conventions, boundaries, and commands.

**Read AGENTS.md at the start of every session.**

Key rules in brief:
- Error handling: `eyre::Result` everywhere in library crates
- Logging: `tracing` macros only, never `println!`
- Never: `unwrap()` in library crates, nightly Rust, modify pinned dep versions
- After every code generation: run `cargo check` before proceeding
- Commit after each verified prompt completion

See AGENTS.md for complete rules, canonical code patterns, and boundary definitions.
See spec.md for architecture, schema reference, and Ethereum addresses.