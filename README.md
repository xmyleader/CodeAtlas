# CodeAtlas

**Turn unfamiliar codebases into evidence-backed mental models.**

CodeAtlas is a read-only repository-understanding agent written in Rust. It addresses a growing onboarding problem: code can be produced and changed faster than people can build an accurate mental model of it. CodeAtlas indexes a repository, lets a tool-calling model explore that index, and requires factual conclusions to link back to source evidence.

CodeAtlas is not a general coding agent. It does not edit files, run repository commands, or attempt to fix code. General agents optimize for completing arbitrary engineering tasks; CodeAtlas productizes one workflow: reusable structural indexing, question-directed exploration, evidence validation, guided progressive explanation, call-path explanation, and deterministic diagrams in a dedicated TUI or native GUI.

## Specialized Design

CodeAtlas has more than the two course-required scenario specializations:

1. **Structural, cross-language index.** Tree-sitter adapters convert Rust, Python, and Python stub files into a language-neutral IR containing files, modules, symbols, imports, references, calls, and entry points. A fingerprinted local cache avoids rebuilding an unchanged index.
2. **Evidence-grounded answers.** Repository tools return stable `Evidence` records. Final answers classify claims as Fact, Inference, or Unknown; the runtime rejects factual claims, call paths, and diagram elements with invalid evidence bindings.
3. **Question-driven read-only exploration.** The model chooses among nine bounded tools (`list_files`, `read_file`, `find_symbol`, `find_references`, `get_symbol`, `get_module`, `search_code`, `trace_call`, and `get_repository_overview`) instead of receiving the whole repository in one prompt.
4. **Safe diagram artifacts.** The model submits typed diagram data, not executable drawing code. A local Rust renderer validates and deterministically writes SVG outside the analyzed repository.
5. **Guided progressive onboarding.** A closed `ExplanationProfile` controls audience and depth without weakening evidence rules. Validated answers receive a small deterministic set of typed `SuggestedAction` values for safe, same-session exploration.

## Guided Progressive Onboarding

The UI-neutral core, Agent, persistence, and application command layers implement explanation profiles and suggested actions. An `ExplanationProfile` combines one audience with one depth:

| Dimension | Values | Behavior |
|---|---|---|
| Audience | `Beginner`, `Developer`, `Expert` | Beginner explanations establish purpose and define advanced terms; Developer assumes general programming knowledge and no repository familiarity; Expert emphasizes invariants, tradeoffs, edge cases, and constraints. |
| Depth | `Auto`, `Overview`, `Architecture`, `Workflow`, `Code`, `Detail` | Auto chooses the minimum sufficient focus; the explicit depths move from orientation through boundaries, runtime flow, implementation mechanics, and deep local detail. |

The backward-compatible default is `Developer` + `Auto`. The profile is converted to a trusted, enum-derived instruction for the current task only; repository text cannot supply profile instructions, and every factual assertion still requires evidence. Each terminal task stores its profile in session schema 3, including history summaries; older records without a profile restore the default.

`SuggestedAction` is a closed, non-executable enum with `DeepenClaim`, `ContinueCallPath`, `ExplainEvidence`, `ShowSource`, and `ChangeDepth` variants. While constructing the final validated answer, the runtime currently derives at most four unique actions: the first factual claim (or first claim), an incomplete call path (or first path), the first retained Evidence item, and the next applicable depth. `ExplainEvidence` is supported by the application contract but is not currently selected by the automatic generator. Labels are fixed by the runtime, referenced IDs must resolve inside the answer, and `RunSuggestedAction` rejects actions that the persisted referenced answer did not offer.

Actions that need another explanation become an `Ask` in the same repository session, preserving prior observable context and the originating audience; `ChangeDepth` changes only the requested depth. `ShowSource` instead becomes a local `LoadSource` request and makes no model call, so it adds no provider tokens or cost.

Both interfaces expose these controls. The GUI places compact Audience and Depth selectors beside the question composer and renders action buttons below the selected answer. In TUI navigation mode, `g` opens profile settings and `A` opens the selected answer's action menu; use arrows or `j`/`k` to navigate and `Enter` to apply or run. Loading or selecting a historical task restores its recorded profile.

## Architecture

```text
Repository (.rs/.py/.pyi)
        |
        v
scanner -> language parsers -> language-neutral IR -> cached index
                                                   |
Question -> Agent runtime -> bounded read-only tools+
                 |                    |
                 |                    v
                 +<---------- tool results + Evidence
                 |
                 v
       structured answer validation -> typed suggested actions
                  |
                  v
             optional local SVG
                 |
                 v
          application event bus
             /         \
        Ratatui TUI   egui GUI
```

### Workspace Map

| Path | Responsibility |
|---|---|
| [`crates/codeatlas-core`](crates/codeatlas-core) | Stable IDs, language-neutral IR, evidence, explanation profiles, suggested actions, model, command/event, and session contracts |
| [`crates/codeatlas-lang-rust`](crates/codeatlas-lang-rust) | Rust tree-sitter parser adapter |
| [`crates/codeatlas-lang-python`](crates/codeatlas-lang-python) | Python and `.pyi` tree-sitter parser adapter |
| [`crates/codeatlas-indexer`](crates/codeatlas-indexer) | Safe scanning, parser dispatch, IR assembly, diagnostics, and JSON index cache |
| [`crates/codeatlas-query`](crates/codeatlas-query) | Deterministic in-memory index and nine bounded read-only Agent tools |
| [`crates/codeatlas-agent`](crates/codeatlas-agent) | OpenAI-compatible client, tool loop, retries, evidence gates, usage aggregation, and session storage |
| [`crates/codeatlas-diagram`](crates/codeatlas-diagram) | Validated deterministic SVG layout and rendering |
| [`crates/codeatlas-app`](crates/codeatlas-app) | Composition, credentials, background commands, persistence, and the two binaries |
| [`crates/codeatlas-tui`](crates/codeatlas-tui) | Ratatui presentation and terminal interaction |
| [`crates/codeatlas-gui`](crates/codeatlas-gui) | Native egui/eframe workbench |
| [`diagram-showcase`](diagram-showcase/README.md) | Offline generated architecture, flow, and relationship examples |

## R1-R6 Compliance Snapshot

This table describes the current executable behavior rather than planned behavior. Provider-dependent limitations are stated explicitly and should remain visible in the submission.

| Requirement | Status | Current behavior and gap |
|---|---|---|
| **R1: Rust implementation** | **Implemented** | The application, agent, indexer, parsers, persistence, TUI, GUI, and diagram renderer are implemented as a Rust workspace. |
| **R2: user interface** | **Implemented** | Dedicated Ratatui and native egui interfaces expose repository maps, conversation, evidence, explanation-profile selection, typed suggested actions, task status, complete history summaries, model-call ledgers, budgets, and cancellation. |
| **R3: configuration** | **Implemented** | Both binaries share endpoint/model, reasoning pass-through, timeout/retry, context-window, explicit pricing, budget, credentials, and data-directory configuration. API keys remain runtime-only. |
| **R4: progress and cancellation** | **Implemented** | Presentation-neutral progress/tool events feed both UIs, and cancellation covers model waits, retry sleeps, tool waits, session-lock waits, and bounded indexing work. Blocking work is cooperatively rather than forcibly terminated. |
| **R5: history** | **Implemented** | Session schema 3 records completed, failed, cancelled, and budget-exceeded tasks with per-task explanation profiles, timestamps, terminal reasons, normalized provider-neutral model requests/responses, system-free continuation messages, full raw tool results, progress, usage, per-call retries, and budgets. It never fabricates or stores private chain-of-thought. |
| **R6: accounting** | **Implemented with provider limits** | Every actual model attempt has a persisted ledger record and optional provider usage/cost. Task/session totals and configured token/cost budgets remain explicit about missing usage; estimates use only user-supplied rates and are not billing reconciliation. |

## Prerequisites

- Rust and Cargo **1.85 or newer** (the workspace uses Rust 2024 edition).
- Network access to an OpenAI-compatible **Chat Completions** endpoint and a model that supports tool calling.
- Read access to the repository being analyzed.
- For the GUI, a native desktop/display environment and platform libraries required by eframe for X11 or Wayland and OpenGL. Linux package names vary by distribution; WSLg input-method notes are in the [Guide](GUIDE.md).

Common Debian/Ubuntu GUI build and portal dependencies can be installed with:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libx11-dev libxi-dev \
  libgl1-mesa-dev libwayland-dev libxkbcommon-dev xdg-desktop-portal
```

## One-Line Install

Install both the `codeatlas` TUI and `codeatlas-gui` native application directly from GitHub:

```bash
cargo install --git https://github.com/xmyleader/CodeAtlas.git --locked codeatlas-app
```

Cargo places both executables in `~/.cargo/bin` by default. Ensure that directory is in `PATH`, then run `codeatlas .` or `codeatlas-gui .` from the repository you want to understand.

## Build And Configure

```bash
cd CodeAtlas
cargo build --workspace --locked
```

For optimized standalone binaries:

```bash
cargo build --release -p codeatlas-app --bins --locked
```

Configuration is shared by the TUI and GUI. The endpoint must be the complete Chat Completions URL, not an API base URL. Start from [`.env.example`](.env.example), replace every placeholder, and load it into the current shell; CodeAtlas does **not** automatically read `.env` files.

```bash
cp .env.example .env
# Edit .env, then:
set -a
. ./.env
set +a
```

Alternatively, store the API key in a local `0600` XDG credentials file so it is not kept in the project environment file:

```bash
cargo run -p codeatlas-app --bin codeatlas -- --store-api-key
```

`CODEATLAS_API_KEY` overrides that file. Non-secret runtime settings are available as flags or environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `CODEATLAS_ENDPOINT` | `https://api.openai.com/v1/chat/completions` | Complete provider URL |
| `CODEATLAS_MODEL` | `gpt-4.1-mini` | Tool-capable model name |
| `CODEATLAS_REASONING_MODE` | Unset | Optional provider-specific reasoning mode |
| `CODEATLAS_REASONING_EFFORT` | Unset | Optional provider-specific reasoning effort |
| `CODEATLAS_TEMPERATURE` | Provider default | Sampling temperature |
| `CODEATLAS_MAX_OUTPUT_TOKENS` | Provider default | Per-response output ceiling |
| `CODEATLAS_CONTEXT_WINDOW_TOKENS` | Unset | Context window used for proactive compaction |
| `CODEATLAS_MODEL_TIMEOUT_SECONDS` | `180` | Timeout for one HTTP model request |
| `CODEATLAS_AGENT_TIMEOUT_SECONDS` | `1200` | Deadline for one complete question |
| `CODEATLAS_MODEL_MAX_RETRIES` | `2` | General transient model retries |
| `CODEATLAS_GATEWAY_MAX_RETRIES` | `5` | HTTP 502/503/504 retries |
| `CODEATLAS_PRICING_CURRENCY` | `USD` | Label for explicit pricing and cost budget |
| `CODEATLAS_INPUT_PRICE_PER_MILLION` | Unset | Uncached input rate; requires output rate |
| `CODEATLAS_CACHED_INPUT_PRICE_PER_MILLION` | Input rate | Optional cached-input rate |
| `CODEATLAS_OUTPUT_PRICE_PER_MILLION` | Unset | Output rate; requires input rate |
| `CODEATLAS_AGENT_MAX_TOTAL_TOKENS` | Unset | Per-task cumulative reported-token limit |
| `CODEATLAS_AGENT_MAX_COST` | Unset | Per-task estimated-cost limit; requires pricing |
| `CODEATLAS_DATA_DIR` | XDG data directory, then HOME, then `codeatlas-<pid>` in the system temp directory | Index, session, and diagram storage; must be outside the analyzed repository |

See the [Guide](GUIDE.md) for all controls, credentials management, history behavior, and troubleshooting.

## Run

TUI, indexing the current repository immediately:

```bash
cargo run -p codeatlas-app --bin codeatlas -- .
```

Use `i` to choose a repository, `a` to ask, `g` to set the explanation profile, `A` to run an offered action, `h` for history, `c` to cancel, and `q` to quit.

Native GUI, also indexing the current repository:

```bash
cargo run -p codeatlas-app --bin codeatlas-gui -- .
```

Omit `.` to start on the welcome screen and choose a directory. The GUI has responsive Repository, Conversation, and Claims & Evidence views, plus session history and cancellation controls.

## Demo Script

After the status reaches `INDEXED` or `Repository ready`, keep one session open and demonstrate **Overview -> Workflow -> Code/Evidence**:

1. **Overview:** `这个项目主要做什么？请用概览方式列出核心能力和少量关键模块，并给出源码证据。`
2. **Workflow:** `在同一会话中，追踪从用户提问到证据校验完成的运行流程；按顺序说明关键分支，并区分事实、推断和未知。`
3. **Code / Evidence:** `继续在同一会话中，下钻到实现这条流程的关键文件、类型和函数，并解释每条源码 Evidence 如何支持结论。` Then inspect one cited source range locally.

Set `Beginner` + `Overview` for the first question, then use the same session with `Beginner` + `Workflow` and `Beginner` + `Code`. Finish by running an offered `ShowSource` action; the last step loads source locally without a model request.

Additional cases:

- **Diagram:** `画出 CodeAtlas 从用户提问到证据校验完成的流程图，每个元素都要有源码依据。`
- **History:** Ask a follow-up, start a new session, then load the earlier session after indexing the matching repository.
- **Cancellation:** Start a broad trace and use the interface's existing cancellation control.

The checked-in diagrams can be regenerated without a provider:

```bash
cargo run -p codeatlas-diagram --example codeatlas_showcase -- ./diagram-showcase
```

## Verification

These commands require no API key and make no live provider request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps --locked
cargo build --release -p codeatlas-app --bins --locked
cargo run -p codeatlas-app --bin codeatlas -- --help
cargo run -p codeatlas-app --bin codeatlas-gui -- --help
```

A real end-to-end answer additionally requires an explicitly configured provider and may incur provider charges.

## Limitations

- Parsing/indexing currently supports only `.rs`, `.py`, and `.pyi`; syntax highlighting recognizes more languages but does not imply indexing support.
- Calls and references are conservative static approximations. Dynamic dispatch, reflection, generated code, macros, and runtime registration can be incomplete.
- Source search is bounded literal search, not embedding-based semantic search.
- Chat Completions requests are non-streaming. Progress and tool events are live, but final answer text arrives after completion.
- Provider usage can be absent; CodeAtlas does not fabricate zero usage or cost. A configured budget stops as unenforceable when usage is missing, and explicit cost estimates are not a provider bill.
- Scanning skips symlinks and non-regular files and applies size/count limits. Files may also be omitted because of ignore rules or parse diagnostics.
- Read-only means the analyzed repository is not modified. CodeAtlas still writes indexes, schema 3 sessions for every terminal task outcome, and diagrams under its external data directory.

## Privacy And Security

Repository tool results can contain source text and are sent with questions, the selected explanation profile instruction, and conversation context to the configured model provider. Session schema 3 also deliberately stores each task's profile and the complete observable model/tool transcript locally, including full source-bearing tool outputs and failures. Do not use CodeAtlas on confidential repositories unless both the provider and local storage are approved for the data.

Read [PRIVACY.md](PRIVACY.md) before use for the exact data flow, storage locations, retention limitations, and complete deletion instructions. The Agent treats repository text as untrusted data, exposes only a fixed read-only tool set, and never executes model-supplied shell or SVG code.

## Manual Course Submission Items

The repository intentionally does **not** generate or include the following submission artifacts:

- Final report PDF: prepare and inspect the final document manually.
- AI development conversation record: export the authentic conversation required by the course; do not substitute a fabricated transcript.
- Expense spreadsheet: use the persisted per-call CodeAtlas ledger as development evidence, then manually reconcile its provider-reported usage and configured price estimates with real provider billing records.

## References

- [Public showcase and trial guide](SHOWCASE.md)
- [Detailed user guide](GUIDE.md)
- [Original project positioning and design rationale](CodeAtlas_PROJECT_IDEA.md)
- [Offline diagram evidence and examples](diagram-showcase/README.md)
- [Tree-sitter documentation](https://tree-sitter.github.io/tree-sitter/)
- [Cargo build reference](https://doc.rust-lang.org/cargo/commands/cargo-build.html)
- [Archify related work](https://github.com/tt-a1i/archify)

## License

CodeAtlas is available under the [MIT License](LICENSE).
