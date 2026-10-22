# dsh-tui-nano

**A minimal, restrained terminal UI for DeepSeek Harness.**

[中文](README.md) · [Project purpose](https://github.com/Strelizialeomon/dsh-tui-nano/issues/1) · [Spec](docs/specs/2026-10-20-nano-v1/README.md) · [Design notes](docs/design/README.md)

> **Alpha, unreleased.** There is no official release package yet; build or run from source. The only verified combination is macOS arm64 + Ghostty (release acceptance uses fullscreen; evidence screenshots may be window captures).<br>
> This is an independent project, not an official DeepSeek product.

![status](https://img.shields.io/badge/status-alpha-orange)
![platform](https://img.shields.io/badge/platform-macOS%20arm64-lightgrey)
![dsh](https://img.shields.io/badge/dsh-0.2.1--rc.2-blue)
![license](https://img.shields.io/badge/license-MIT-blue)

<p align="center">
  <img src="docs/evidence/2026-10-22-visual/batch1/03-dark-100-response.png" alt="nano running in Ghostty: conversation, context, and composer" width="900">
</p>

<p align="center"><sub>Real Ghostty screenshot: 100 columns, dark theme.</sub></p>

<details>
<summary>More real terminal screenshots</summary>

<p align="center">
  <img src="docs/evidence/2026-10-22-visual/batch1/02-dark-100-running.png" alt="A running turn and the activity line" width="440">
  <img src="docs/evidence/2026-10-22-visual/batch1/08-dark-100-diff-not-applied.png" alt="File diff with line numbers" width="440">
</p>

</details>

> This is an English mirror. If the two versions diverge, the Chinese README is authoritative.

## Contents

- [What is this](#what-is-this)
- [Features](#features)
- [Requirements](#requirements)
- [Quick start](#quick-start)
- [Key bindings](#key-bindings)
- [Status](#status)
- [Known limitations](#known-limitations)
- [Development](#development)
- [Architecture](#architecture)
- [Contributing](#contributing)
- [License](#license)
- [Acknowledgements](#acknowledgements)

## What is this

[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) (`dsh`) owns the agent, models, tools, sessions, permissions, and persistence. nano does not reimplement any of that; it is only the terminal interface:

- **Rust** owns terminal interaction, reading, input, and rendering.
- **A thin TypeScript bridge** runs inside the official profile and translates public APIs into a versioned protocol.
- The two sides exchange a versioned local protocol, so upstream changes mostly hit the bridge instead of being scattered across the UI.

The design goals are simple: **reading first, low decoration, and no invented facts**. Sessions, permissions, models, and tool results always belong to the official DSH; nano only displays receipts and state. nano is an independent rewrite and does not inherit the old dsh-TUI fork's implementation or Git history.

## Features

- **Conversation**: multiline input, streaming output, queue / steer / stop, per-address drafts, resume.
- **Reading and decisions**: tool calls, file diffs, questionnaires, approvals, plan cards; process details stay low-emphasis and expand on demand.
- **Input**: `@file` references, image / generic file attachments, skill catalog, instruction sources.
- **Workflow**: `!` manual command, job panel, interactive terminal, Monitor output watching.
- **Sessions and control**: session list / resume / rename, context pressure, `/compact`, schedules, subagents.
- **Models and permissions**: `/model` for model and reasoning effort, `/permission` for presets, `Ctrl+P` for the command palette.

Features are implemented per the specs. The first visual-alignment batch is merged; the second batch is pending. Version 0.1.0 has not been released.

## Requirements

| Item | Requirement |
| --- | --- |
| OS / terminal | macOS arm64 + Ghostty (the only verified combination; release acceptance uses fullscreen; evidence screenshots may be window captures; others are untested) |
| Official dsh | Exactly `0.2.1-rc.2`; unknown versions are refused at startup |
| Node | Major version ≥ 22 (verified with v24.18.0) |
| Rust | stable (`rust-version = 1.82`) |
| Credentials | Resolved by official dsh in `${DSH_HOME:-~/.dsh}`; nano never copies them |

The first launch installs the nano profile through `dsh plugin`, which invokes pnpm internally.

## Quick start

### From source

```sh
git clone https://github.com/Strelizialeomon/dsh-tui-nano.git
cd dsh-tui-nano

# Bridge dependencies (official public exports)
npm install --prefix bridge

# The first launch installs the nano profile automatically
cargo run -p nano-app
```

To pick an initial model (uses official credentials and incurs model usage):

```sh
cargo run -p nano-app -- --provider deepseek-official --model deepseek-chat
```

The first launch installs the profile under `${DSH_HOME:-~/.dsh}/profiles/dsh-tui-nano`; nano's own data lives in `~/.dsh-tui-nano`. See `nano --help` for all options and keys.

### From a local package

There is no GitHub Release yet. Build and install locally:

```sh
./packaging/build-release.sh
cd packaging/dist/dsh-tui-nano-0.1.0-arm64
./install.sh
```

See [`packaging/README.md`](packaging/README.md) for packaging details. The 0.1.0 release acceptance is not closed; do not redistribute local builds.

## Key bindings

| Key | Action |
| --- | --- |
| `Enter` / `Alt+Enter` | Send / queue; steer |
| `Esc` | Stop the current turn (keeps the queue) |
| `Ctrl+J` | Newline in the composer |
| `Ctrl+P` | Command palette |
| `/model` / `/permission` | Model and effort / permission preset |
| `Alt+O` / `Alt+C` | Session list / compact context |
| `Alt+K` / `Alt+R` | Skill catalog / instruction sources |
| `@` / `Tab` | Reference a file / accept completion |
| `Alt+A` | Attach an image or file |
| `Ctrl+T` / `F12` | Jobs / interactive terminal |
| `Ctrl+G` / `Alt+L` | Schedules / subagents |
| `PageUp` / `PageDown` / `Ctrl+End` | Scroll / jump to bottom |
| `Ctrl+C` / `Ctrl+Q` | Quit |

`nano --help` lists the full keymap. `!command` runs with your user privileges, not the agent's; its output is injected as synthetic context without waking the model.

## Status

| Item | Status |
| --- | --- |
| Feature components S01–S12 | Implemented |
| Visual alignment | First batch merged; second batch pending (light diff, questionnaire / approval cards, etc.) |
| 0.1.0 release | **Unreleased**; release acceptance still has open items |
| Platform | Only macOS arm64 + Ghostty verified; release acceptance uses fullscreen |

See [`docs/release/0.1.0/`](docs/release/0.1.0/README.md) for blockers, evidence, and exit conditions. The release is blocked by open acceptance items; see the release directory for the specific blockers and evidence.

## Known limitations

- Only macOS arm64 + Ghostty is claimed; release acceptance uses fullscreen, and evidence screenshots may be window captures; other systems, terminals, and architectures are unverified.
- Only official dsh `0.2.1-rc.2` is accepted; unknown versions are refused at startup, with no field guessing or fallback.
- Schedules have no pause / resume API; stopping a task means deleting it. Delivery is not exactly-once and may repeat.
- Monitor `notify` wakes the model and incurs cost; `quiet` does not.
- `@file` only inserts a path; the model still needs a file tool to read the content.
- Credentials are resolved only inside official dsh; nano does not copy or modify credentials, global instructions, or `storages/` under `$DSH_HOME`.
- Descendant processes from hard kills, power loss, or deliberate escapes are not guaranteed to be cleaned up; nano only owns the process group of its own window.

Full list: [`docs/release/0.1.0/known-limitations.md`](docs/release/0.1.0/known-limitations.md).

## Development

```sh
npm install --prefix bridge   # Bridge dependencies
npm run test:bridge           # Contract + bridge unit tests (no credentials)
npm run test:rust             # Rust workspace

# Real-host integration tests: installs the profile, starts real dsh, may incur cost
cargo test -p nano-runtime --test integration -- --ignored --test-threads=1 \
  --skip shutdown_budget_is_bounded_when_cleanup_hangs
```

See [`tests/e2e/README.md`](tests/e2e/README.md) and [`docs/release/0.1.0/README.md`](docs/release/0.1.0/README.md) for cross-feature e2e, real Ghostty acceptance, and release checks. Real PTY evidence scripts for each feature live in `scripts/`; read the header of each script for prerequisites. All integration tests use temporary `DSH_HOME` and data roots; they do not read or write your real sessions.

## Architecture

```text
Ghostty
  └─ nano (Rust TUI)
       └─ dsh CLI + nano profile
            ├─ bridge (TypeScript): protocol, boundary checks, cleanup
            └─ official DSH: agent / models / tools / sessions / permissions / persistence
```

| Path | Contents |
| --- | --- |
| `crates/nano-app/` | The `nano` terminal app: keys, rendering, host link, exit |
| `crates/nano-*` | Feature state and projections (conversation, reading, sessions, schedules, Monitor, etc.) |
| `bridge/` | Thin TypeScript bridge running inside the official profile |
| `protocol/` | Machine-checkable versioned protocol contract |
| `packaging/` | macOS arm64 packaging, install, and uninstall |
| `docs/specs/` | Specifications and acceptance criteria |
| `docs/release/` | Release acceptance, evidence, and known limitations |

More detail: [`docs/specs/2026-10-20-nano-v1/README.md`](docs/specs/2026-10-20-nano-v1/README.md) and [`protocol/README.md`](protocol/README.md).

## Contributing

This is an early, individual-led project. Issues and PRs are welcome. Please open an issue to align on scope before a large change.

- Run `npm run test:bridge` and `npm run test:rust` before submitting.
- Changes touching the real host, terminal, or visuals should include real evidence (command output, screenshots, or recordings).
- There is no CI yet; review is manual. Keep changes small and well explained.

## License

MIT. The license text currently lives in [`Cargo.toml`](Cargo.toml) (`license = "MIT"`); a standalone `LICENSE` file is still pending. Confirm the license file before reuse or distribution.

## Acknowledgements

- [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness): agent, models, tools, sessions, and persistence.
- [Ratatui](https://github.com/ratatui/ratatui) / [Crossterm](https://github.com/crossterm-rs/crossterm): Rust terminal rendering and events.
- The design mockups and real-terminal evidence contributors.
