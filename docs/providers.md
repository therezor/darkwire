# Providers

A provider is data; only a wire protocol is code. The registry is a table, and adding an
OpenAI-compatible endpoint means adding a row — or not even that, since `custom` takes any
base URL.

## The registry

| Type         | Wire               | Default base                                              | Env key              | Local | Notes                                  |
| ------------ | ------------------ | --------------------------------------------------------- | -------------------- | :---: | -------------------------------------- |
| `ollama`     | openai-chat        | `http://127.0.0.1:11434/v1`                               | —                    |  ✅   | Detected by port                       |
| `lmstudio`   | openai-chat        | `http://127.0.0.1:1234/v1`                                | —                    |  ✅   | Detected by port                       |
| `llamacpp`   | openai-chat        | `http://127.0.0.1:8080/v1`                                | —                    |  ✅   |                                        |
| `vllm`       | openai-chat        | `http://127.0.0.1:8000/v1`                                | `VLLM_API_KEY`       |  ✅   |                                        |
| `openrouter` | openai-chat        | `https://openrouter.ai/api/v1`                            | `OPENROUTER_API_KEY` |       | Gateway. Key prefix `sk-or-`. Caching. |
| `openai`     | openai-chat        | `https://api.openai.com/v1`                               | `OPENAI_API_KEY`     |       | Uses `max_completion_tokens`. Caching. |
| `anthropic`  | anthropic-messages | `https://api.anthropic.com/v1`                            | `ANTHROPIC_API_KEY`  |       | **See the note below.** Caching.       |
| `gemini`     | openai-chat        | `https://generativelanguage.googleapis.com/v1beta/openai` | `GEMINI_API_KEY`     |       | Google's OpenAI-compatible layer       |
| `deepseek`   | openai-chat        | `https://api.deepseek.com/v1`                             | `DEEPSEEK_API_KEY`   |       | Caching                                |
| `groq`       | openai-chat        | `https://api.groq.com/openai/v1`                          | `GROQ_API_KEY`       |       | Key prefix `gsk_`                      |
| `xai`        | openai-chat        | `https://api.x.ai/v1`                                     | `XAI_API_KEY`        |       |                                        |
| `custom`     | openai-chat        | _none — you must set `apiBase`_                           | `OPENAI_API_KEY`     |       | Only selected by name                  |

Every type except `anthropic` answers `GET /models`, so the UI's model question is a list
rather than a text box.

**Only the `openai-chat` adapter exists.** Four wire protocols are named
(`openai-chat`, `anthropic-messages`, `gemini-generate`, `openai-responses`) and one is
implemented. Selecting a wire that has no adapter is a loud configuration error, not a
silent fallback: `create_provider` refuses at construction rather than letting a
misconfiguration surface as a 404 mid-turn. Reaching one of those providers today means
pointing an instance at an endpoint that speaks `openai-chat`.

## Types and instances

`config.providers` is keyed by an **instance id you choose**, with `type` naming a
registry row. The same type can appear more than once, which is the only way to express
two Ollama servers:

```json
{
  "providers": {
    "ollama": { "type": "ollama" },
    "ollama-gpu": {
      "type": "ollama",
      "label": "GPU box",
      "apiBase": "http://gpu.lan:11434/v1"
    }
  }
}
```

The instance id is also the vault key for that instance's credential, so the two entries
above can hold different tokens. A local endpoint may carry one too — for a model server
behind an authenticating proxy.

A `config.yaml` written before instances existed is migrated on load and rewritten in
place: each key keeps its name and gains the matching `type`, so credentials already in
the vault keep resolving.

## Resolution

An agent's `provider` takes one of three forms:

1. **An instance id.** Exact, and the common case.
2. **A bare provider type.** Means "any enabled instance of that type, or a default one if
   none is configured", which is what keeps `ghostai chat --provider ollama` working on a
   machine with no config file.
3. **`auto`.** Runs the resolution order: gateway and local detection by key prefix or
   base URL, then model-name keywords.

`auto` returns `null` rather than guessing when nothing matches. An empty `model` is a
separate condition — it means _unconfigured_, and every turn is refused with a message
saying so. There is no model-picking code anywhere; the setup wizard's model step is
skippable, and the UI treats an empty model as a question to answer rather than as
"choose for me".

Disabled instances are skipped by both resolution and model listing, but kept in the file.

## Credentials

Keys never appear in `config.yaml`. They live in the encrypted vault under the namespace
`providers`, keyed by instance id.

**The vault wins over the environment.** An env var is consulted only when the vault has
no entry for that instance — so a key set in the UI is not silently shadowed by a stale
shell export. The vault is opened only if `vault.json` already exists, so a local-only
install never creates a keychain entry it did not need.

Over HTTP the vault is write-only: `PUT /api/settings/credentials` stores one, and nothing
reads one back out. What a client can see is a per-instance `credentialsPresent` boolean.

The provider base URL deliberately does **not** go through the guarded fetch — the common
case is loopback, which the SSRF guard exists to refuse. What is enforced instead is
narrower and matches the actual risk: an API key is never sent over plain HTTP to a public
address.

## Resilience

`with_resilience` decorates both streaming and non-streaming calls. Its retry ladder is
declarative, six steps, and the order _is_ the policy — each step costs more than the one
above it. It drops `prompt_cache_key`, then merges the trailing turn, then drops
`reasoning_effort`, then `tool_choice`, then images, then the oldest turns: a cache-routing
hint costs nothing, a message boundary costs little, `reasoning_effort` costs answer
quality, an image costs information the user supplied, and a turn costs the conversation's
memory. Each is tried only after the one above it has failed or does not apply.

**A stream that has already emitted output is never retried.** Retrying it would either
duplicate text the user has read or silently replace it.

Errors are typed. `ProviderError` carries a `reason`, and nothing in the codebase branches
on a substring of a provider's message — a `notice` with kind `provider_fallback` or
`degraded` tells the operator when a request was narrowed to succeed.

## Token accounting

One estimator, deliberately: `ceil(length / 4)`, with no tables and no allocation. It
answers one question — "roughly how big is this?" — which the degradation ladder asks when
deciding how many turns to drop after a context-length rejection. That runs on the failure
path, so it must not be slower than the success path.

**There is no exact tokenizer, and that is a decision rather than a gap.** A per-provider
one would mean shipping megabytes of vocabulary per provider to sharpen an estimate that
exists to decide _whether_ to truncate, not exactly where. Nothing in the tree sizes
against a hard limit, so nothing needs one. What the estimate is applied to matters more
than its precision: it measures the wire-encoded body rather than the stored records, so
nothing the provider never receives is counted.

Cached-prompt tokens are read back from the response and surfaced per turn, so the effect
of the [prompt caching split](prompts.md#two-halves-and-why) is visible in the UI's turn
info rather than merely asserted here.
