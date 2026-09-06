# CodeAtlas Privacy Notice

CodeAtlas analyzes local source code but is not an offline application when a question is asked. This document describes the current implementation; the policy and retention terms of your configured model provider also apply.

## Data Sent To The Provider

For each model call, CodeAtlas sends the configured endpoint:

- the CodeAtlas system instructions and read-only tool definitions;
- the current question;
- the selected session's prior observable user, assistant, and tool messages (without replaying its old system message);
- model-generated tool calls and tool results from the current question;
- repository paths, symbol/module metadata, call/reference data, and source excerpts included in those tool results;
- validation or protocol errors used to request one supported repair.

CodeAtlas does not upload the entire repository as a single payload. The model selects bounded tools, but their results can contain sensitive source text. A multi-step question and retries can send multiple requests or resend context. The HTTP request also contains the configured model name, optional provider-specific reasoning settings, and API authorization header. Explicit pricing rates and local budget amounts are not part of the model request.

CodeAtlas does not intentionally send its local credentials file, index/session file paths, unrelated file types, or private chain-of-thought. It cannot guarantee that source files themselves do not contain secrets. Review and approve the endpoint before analyzing proprietary or regulated code. Provider-side logging, training, retention, location, access control, and deletion are governed by the provider, not by CodeAtlas.

## Data Kept Locally

The default data root is `$XDG_DATA_HOME/codeatlas`, or `$HOME/.local/share/codeatlas` when `XDG_DATA_HOME` is unset. If neither XDG nor `HOME` is available, the application uses `codeatlas-<process-id>` under the operating system temporary directory. That last location is process-specific and is not durable across cleanup or restart. `CODEATLAS_DATA_DIR` overrides all defaults. The current layout is:

```text
<data-root>/indexes/    structural index caches (paths, symbols, and relationships)
<data-root>/sessions/   complete schema 3 task records and observable transcripts
<data-root>/diagrams/   generated SVG artifacts
```

Indexes do not copy full source files; query tools read indexed files from the repository when needed. Session schema 3 deliberately records every terminal task (`completed`, `failed`, `cancelled`, or `budget_exceeded`) with its question, request ID, timestamps, optional answer, terminal error/reason, normalized provider-neutral requests passed to `ModelClient`, parsed provider-neutral responses, system-free continuation messages, retries, progress, usage ledger, and budget events. Each `ToolCallCompleted` stores the full raw JSON result returned by the tool executor, including source-bearing `content`, `excerpt`, and `line` fields without persistence redaction or truncation. The separately stored tool message used for continuation can be smaller because evidence deduplication and outbound context-window compaction apply there. Session files do not capture HTTP wire bytes or response fields discarded by the provider adapter. UI views may summarize stored data. No private chain-of-thought is requested, invented, or persisted. Local records have no automatic expiry and remain until deleted.

Authentication headers and API keys are held separately by the HTTP client and are not fields of the serializable model request or session schema. Provider error diagnostics are secret-redacted before they can enter the trajectory. Repository files or user questions can themselves contain secrets, so the session directory must still be treated as sensitive.

The optional API key is separate at `$XDG_CONFIG_HOME/codeatlas/credentials.json`, or `~/.config/codeatlas/credentials.json`. On Unix it is local plaintext restricted to mode `0600`; `CODEATLAS_API_KEY` takes precedence. Keep the host account and backups secure.

CodeAtlas contains no application telemetry or analytics client. Normal provider requests and opening a diagram with the operating system's default viewer are still external interactions.

## Delete Local Data

Close all CodeAtlas processes first. Resolve the active data root using the same precedence as the application, inspect it, and delete the desired records:

```bash
if [ -n "${CODEATLAS_DATA_DIR:-}" ]; then
  DATA_ROOT="$CODEATLAS_DATA_DIR"
elif [ -n "${XDG_DATA_HOME:-}" ]; then
  DATA_ROOT="$XDG_DATA_HOME/codeatlas"
elif [ -n "${HOME:-}" ]; then
  DATA_ROOT="$HOME/.local/share/codeatlas"
else
  printf '%s\n' 'No stable default: inspect ${TMPDIR:-/tmp}/codeatlas-<process-id> for the CodeAtlas process that created it.' >&2
  exit 1
fi
printf 'CodeAtlas data root: %s\n' "$DATA_ROOT"

# Delete conversation history only:
rm -rf -- "$DATA_ROOT/sessions"

# Delete all local indexes, sessions, and generated diagrams:
rm -rf -- "$DATA_ROOT"
```

These commands do not remove copies in backups, exported files, operating-system caches, or provider systems. Follow the provider's deletion procedure separately. If a custom `CODEATLAS_DATA_DIR` was used, run the commands with that same value.

Delete the stored API key with:

```bash
cargo run -p codeatlas-app --bin codeatlas -- --delete-stored-api-key
```

Also remove any API key from shell startup files, `.env`, CI settings, password managers, or other locations where you placed it. Deleting local data is irreversible.
