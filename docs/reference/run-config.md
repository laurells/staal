# Run config

A run config is a JSON object that describes one run: which function to run, its
inputs, the authority it is granted, and the services it uses. `staalc run --config`,
`resume`, `profile` and `debug` read it from a file; the Python, Node.js and C
interfaces take it as a value. `staalc config <file.staal> --entry <function>` prints
one to fill in.

The whole config is checked before the run starts. A missing or unknown key, or a value
of the wrong type, is an error, and nothing runs (exit code 2).

```json
{
  "entry": "handle",
  "inputs": { "order": "4417", "amount": { "amount": "35.00", "currency": "USD" } },
  "grants": { "pay": { "cap": "payments.refund", "each": { "amount": "50.00", "currency": "USD" } } },
  "secrets": { "key": { "env": "PAYMENTS_KEY" } },
  "model": { "provider": "anthropic", "model": "claude-haiku-5-5", "api_key": { "env": "ANTHROPIC_API_KEY" } },
  "limits": { "max_steps": 1000000, "deadline_ms": 60000 }
}
```

## Top-level keys

| Key | Required | Contents |
| --- | --- | --- |
| `entry` | yes | The function to run. A `pub` function of an imported module is named by its module path, such as `"sms.termii.send"`. |
| `inputs` | if the entry has inputs | Every parameter of the entry function that is not a capability or a `Secret`, by name. A parameter with a default may be left out. |
| `grants` | if the entry has capability parameters | A grant for every capability parameter, by name. |
| `secrets` | if the entry has `Secret` parameters | Where each `Secret` parameter is read from, by name. |
| `model` | if the run calls a model | The model provider. Without it, a model call fails with kind `provider` before anything is sent. |
| `adapters` | no | Settings of the simulated service that performs actions with no real service configured. |
| `limits` | no | Step, depth, token and time limits. |
| `net` | no | Settings of the runtime's network client. |
| `fs` | no | Settings of the runtime's file capabilities. |

An entry parameter cannot have type `Proposed<T>`, `Receipt`, `Outcome` or `Done`:
only the runtime produces those.

```staal file=greet.staal
fn greet(name: Text) -> Text {
    return "Hello, " + name
}
```

```json file=typo.json
{ "entry": "greet", "inputs": { "name": "Ada" }, "limit": { "max_steps": 100 } }
```

```console
$ staalc run greet.staal --config typo.json
staalc: run config: unknown key 'limit'
```

## `inputs`

Each value is the JSON form of the parameter's type, which must match exactly:

| Type | JSON |
| --- | --- |
| `Text` | a string |
| `Int` | an integer |
| `Bool` | `true` or `false` |
| `Money` | `{"amount": "12.50", "currency": "USD"}`: the amount a decimal string, with at most the currency's decimal places |
| record | an object with exactly the record's fields |
| enum | an object with `"kind"` naming the variant, and exactly its fields |
| `List<T, N>` | an array of at most `N` items |
| `Map<K, V, N>` | an object; keys are the `Text` or `Int` keys written as strings |
| `Set<T, N>` | an array of distinct items |
| `Optional<T>` | the value, or `null` |
| tuple | an array of exactly its length |

Currencies: USD, EUR, GBP, NGN, GHS, KES, ZAR, CAD and AUD with two decimal places, and
JPY with none.

## `grants`

```json
{ "pay": { "cap": "payments.refund", "max": { "amount": "200.00", "currency": "USD" }, "uses": 5 } }
```

| Key | Contents |
| --- | --- |
| `cap` | The capability's path. It must be the one the parameter's type names. |
| `max` | The most money spent in total over the run, as money. |
| `each` | The most money one action may spend, as money. |
| `uses` | How many actions may be attempted. A failed attempt counts. |
| `only` | The targets that may be acted on: a list of exact texts or patterns with one `*`. For `net.http.*`, URL patterns. |
| `idempotency_key` | `true` to send each action's idempotency key to the service, where the capability supports it. |
| `needs` | For a library capability: a value for each of its needs. A capability need is a grant; a `Secret` need is `{"env": NAME}`; a `Text` need is a string. |
| `private_addresses` | For a capability that connects (`net.http.*`, `net.tcp.exchange`, `net.tls.exchange`): the private IP addresses and networks it may reach. Without it, only public addresses are reached. |
| `cloud_metadata` | For a capability that connects: `true` to allow cloud metadata addresses. |

If the parameter's type declares limits, the grant is narrowed to them when the run
starts: the effective limit is the stricter of the two.

Capabilities for C functions (`ffi.*`) also take `library`, `sha256`, `repeatable` and
`isolation`; `db.postgres.*` takes `databases`; `net.tcp.accept` and `net.tls.accept`
take `wait_ms` and `on_bad_client`, and `net.tls.accept` also `certificate_chain` and
`private_key`. These are specified in [RUNTIME.md](../RUNTIME.md), section 12.

## `secrets`

```json
{ "key": { "env": "PAYMENTS_KEY" } }
```

A secret is read only from an environment variable, and cannot be given in `inputs`.
Its value must be at least 8 characters of printable ASCII, without spaces, quotes or
backslashes. The log and the journal record a secret by its variable name only, and
every secret value is replaced by `[secret removed]` in every response body, file read
and log event.

## `model`

### Claude

```json
{ "provider": "anthropic", "model": "claude-haiku-5-5", "api_key": { "env": "ANTHROPIC_API_KEY" } }
```

| Key | Default | Contents |
| --- | --- | --- |
| `provider` | | `"anthropic"` |
| `model` | | The Claude model id. |
| `api_key` | | `{"env": NAME}`: the environment variable holding the key. |
| `base_url` | `https://api.anthropic.com` | The API's address: HTTPS, or a local address. |
| `timeout_ms` | 600000 | Time limit for one request. |
| `connect_timeout_ms` | 10000 | Time limit for connecting. |
| `retries` | 2 | Retries after a rate limit or overload (statuses 429, 500, 502, 503, 504, 529), 0 to 10. |
| `retry_delay_ms` | 1000 | The wait before the first retry, at most 60000. It doubles for each retry after; a `Retry-After` from the API replaces it. No wait exceeds 60 seconds. |
| `proxy` | `"none"` | `"environment"` to use the first proxy set in `ALL_PROXY`, `all_proxy`, `HTTPS_PROXY`, `https_proxy`, `HTTP_PROXY` or `http_proxy`, with `NO_PROXY` or `no_proxy`. A proxy with credentials, or a SOCKS proxy, is refused. |
| `extra_root_certificates` | | PEM files of certificates trusted in addition to the built-in roots. |

If the API stays unavailable after its retries, the run stops with `in_doubt` (exit
code 3), and `resume` makes the call again.

### Scripted

```json
{ "provider": "script", "responses": [{ "output": { "kind": "Decline", "reason": "No fault." }, "output_tokens": 40 }] }
```

Each model call takes the first unused response that fits it. A scripted model sends
nothing over the network; it is for tests and examples.

| Key of a response | Contents |
| --- | --- |
| `output` | The model's answer, as JSON. |
| `error` | Instead of `output`: the call fails with this message, kind `provider`. |
| `unavailable` | Instead of `output`: the provider is unavailable, with this message, as when the API stays unavailable after its retries: the run stops in doubt. |
| `output_tokens` | Output tokens charged (default 1). |
| `input_tokens` | Input tokens the request is counted as using. |
| `when` | The response is used only for a request whose JSON contains this text. |

A response has exactly one of `output`, `error` and `unavailable`.

## `adapters`

```json
{ "simulated": { "fail": [{ "cap": "email.send", "when_arg": "bounce@example.com" }] } }
```

The simulated service performs actions of capabilities that reach no real service and
returns receipts (`sim-1`, `sim-2`, ...). The amount it reports for an action is the
first `Money` among its arguments, and the target the first `Text`, searched depth
first.

| Key of `simulated` | Contents |
| --- | --- |
| `fail` | A list of `{"cap", "when_arg"}`: the action fails whenever any of its arguments is exactly `when_arg`. |
| `world` | A file in which performed actions are recorded, one JSON object per line, kept across runs. |
| `idempotent` | Default `true`: an action repeated with the same idempotency key returns the original receipt. |

## `limits`

| Key | Default | Contents |
| --- | --- | --- |
| `max_steps` | 1000000 | Statements executed and loop iterations. Exceeding it fails the run (`step_limit`). |
| `max_depth` | 256 | Nested function calls, at most 2000. Exceeding it fails the run (`depth_limit`). |
| `default_max_tokens` | 1024 | The output token limit of a model call without `tokens:`. |
| `deadline_ms` | none | How long, from the start of this run or resume, the run may keep starting actions and model calls. It is checked before each starts, never during one: past it, the run fails (`deadline`). |

## `net`

| Key | Default | Contents |
| --- | --- | --- |
| `timeout_ms` | 30000 | Time limit for one request. |
| `connect_timeout_ms` | 10000 | Time limit for connecting. |
| `max_response_bytes` | 4194304 | The largest response body read, at most 16777216. |
| `proxy` | `"none"` | `"environment"`, as for `model`. |
| `extra_root_certificates` | | PEM files of certificates trusted in addition to the built-in roots. |

## `fs`

| Key | Default | Contents |
| --- | --- | --- |
| `max_read_bytes` | 16777216 | The largest file `fs.read` reads, at most 16777216. |
