# Mock Binding — `dev.mcpg.backend.mock`

> class `backend` · `native` · package `mcpg-plugin-backend-mock` · artifact `libmcpg_plugin_backend_mock.so` · Apache-2.0

A backend that answers every call with a response the operator wrote in config,
without touching the network, the filesystem, or a subprocess. It carries no
transport machinery at all: registration stores the configured value and
dispatch hands it back, optionally after a simulated delay, optionally as a
simulated tool error, optionally verbatim as a full MCP `CallToolResult`. Reach
for it when you need a tool that exists and behaves predictably before the real
system does — quickstarts, examples, contract tests, offline demos, and thin
passthrough wiring for content shapes (image, audio, embedded resource) that a
transform step produces but the ordinary wrapping path cannot express.

## What it does
- Returns a configured JSON `response` as a text content block plus a
  `structuredContent` record carrying the tool name, profile, binding kind, the
  call arguments, the configured response, and `simulated: true`.
- `passthrough: true` treats `response` as a literal `CallToolResult` and
  surfaces it unchanged, so a binding can emit image / audio /
  embedded-resource / mixed-content arrays.
- `error: true` produces a tool-level failure with `isError: true` and
  `error_message` (or `mock error`) as the body; it takes precedence over
  `passthrough` when both are set.
- `delay_ms` sleeps before responding, so client timeout handling can be
  exercised deterministically.
- Emits its result under the host's verbatim-result envelope
  (`__mcpg_verbatim_result`), which is why the operator — not the gateway's
  default projection — controls `content` and `isError` exactly.
- Declares no required capabilities: it makes no host calls and opens no
  sockets, so its `plugins[]` entry needs no `granted_capabilities`.
- Is excluded from the gateway's LLM child-tool routing table, alongside
  `command`, `openapi`, and `pipeline` — a mock binding cannot be invoked from
  a model's tool loop.

## Configuration
Per-call config lives in each binding's `backend: { kind: mock, … }` block under
`mcp.capabilities.tools[]` (or `prompts[]` / `resources[]`). The plugin is also
linked into the gateway binary: when a config declares at least one `kind: mock`
binding and no `plugins[]` row claims `dev.mcpg.backend.mock`, the gateway
registers the built-in copy and its per-binding profiles itself, so a quickstart
config works with no artifact to build or install. Declaring the cdylib in the
flat top-level `plugins:` list takes over from that fallback.

```yaml
mcp:
  capabilities:
    tools:
      - name: ping
        description: Always returns ok.
        backend:
          kind: mock
          response: { status: ok }
          delay_ms: 0
          error: false
          passthrough: false
```

To load the artifact instead of the in-binary copy:

```yaml
plugins:
  - id: dev.mcpg.backend.mock
    class: backend
    kind: native
    source:
      path: ./plugins/libmcpg_plugin_backend_mock.so
      # or, platform-agnostic:
      # oci: ghcr.io/mcpg-dev/source-code/plugins/backend-mock:protocol-1
```

| Field | Type | Default | Description |
|---|---|---|---|
| `response` | JSON | `null` | The value returned. Stringified into a text block by default; treated as a literal `CallToolResult` under `passthrough`. |
| `delay_ms` | u64 | `0` | Milliseconds to sleep before responding. |
| `error` | bool | `false` | Emit a simulated tool error (`isError: true`) instead of a success. |
| `error_message` | string | unset | Body of the simulated error. Defaults to `mock error`. |
| `passthrough` | bool | `false` | Surface `response` unchanged as the tool result. Requires `response` to be an object carrying a `content` array; anything else is rejected at registration with an invalid-spec error. |

## Response envelope
By default the result is one text block holding the pretty-printed `response`,
and `structuredContent` carries `toolName`, `profile`, `bindingKind: mock`,
`arguments`, `response`, `delayMs`, and `simulated: true`. In error mode the
text block is the error message and `structuredContent` replaces
`response`/`delayMs` with `error`. Under `passthrough` neither wrapper is
applied — what you wrote is what the client receives. Audit records for every
mode carry `mock.transport: plugin`.

## MCP surfaces & composition

### As a pipeline step
`mock` is pipeline-capable, so it can stand in for a not-yet-built step inside a
`kind: pipeline` binding. The step's `kind` names the plugin and every sibling
key is the backend spec. A mock step's output is the configured `response`
itself — no envelope is wrapped around it — so a later step reads it straight
off `steps.<id>.output`. `delay_ms` and `error` apply here too; `passthrough`
has no meaning mid-pipeline, since the step result is a value rather than a
client-facing tool result.

```yaml
      backend:
        kind: pipeline
        steps:
          - id: fetch
            kind: mock
            response: { account_id: "acct-1", tier: "gold" }
          - id: shape
            kind: transform
            expression: "{ 'tier': steps.fetch.output.tier }"
```

### As a resource
Place the binding under `mcp.capabilities.resources[]`. The gateway parses the
returned text body as JSON and requires an MCP `contents` array, so the
configured `response` is that read body directly.

```yaml
  capabilities:
    resources:
      - name: fixture.readme
        description: A static fixture document.
        uri: "mock://fixtures/readme"
        backend:
          kind: mock
          response:
            contents:
              - uri: "mock://fixtures/readme"
                mimeType: text/plain
                text: "hello from the mock backend"
```

### As a prompt
Under `mcp.capabilities.prompts[]` the same rule applies with a `messages`
array, validated against the MCP content-block shape.

```yaml
  capabilities:
    prompts:
      - name: fixture.greeting
        description: A canned prompt.
        backend:
          kind: mock
          response:
            messages:
              - role: user
                content: { type: text, text: "Summarise the account." }
```

### Schemas & annotations
The plugin derives no schema — it has no upstream contract to read one from.
Declare `input_schema`, `output_schema`, and `annotations` on the capability
entry itself when a fixture tool needs to advertise them.

## Build
Default feature set is OFF (avoids `mcpg_plugin_register` linker
collisions in the workspace build); opt in to the cdylib:

```bash
cargo build -p mcpg-plugin-backend-mock --features cdylib-export --release   # → target/release/libmcpg_plugin_backend_mock.so
```

## Sign & load (production)
Sign the artifact, pin/verify via the entry's `signature:` block, and honour
revocations. See <https://mcpg.dev/docs/security/plugin-security>.

## See also
- Backend binding reference: <https://mcpg.dev/docs/reference/backends>
- Pipeline step kinds: <https://mcpg.dev/docs/reference/pipeline-steps>
- Real transports to graduate a mock binding onto: `libs/plugins/backend/http`,
  `libs/plugins/backend/command`, `libs/plugins/backend/sql`
