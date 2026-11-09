# Known limitations and accepted risks

What Staal does not protect against, by design or not yet, so that a reviewer does
not spend time finding them and a user is not surprised by them. Each is stated in
the specification it comes from; this file gathers them. A reviewer who thinks one
of these should not be accepted is asked to say so (SCOPE.md).

## 1. By design: the edges of the model

From [GUARANTEES.md](../GUARANTEES.md) section 4:

- **A model's choice within its authority.** A turned model can take any action the
  grants allow, as often as the limits allow. Staal bounds the worst case; it does
  not make the model's judgement good.
- **What the model writes where it may write.** A free-text channel the program
  gives a model (a message to a customer) can carry misleading text.
- **A model obeying text inside data.** E006 keeps untrusted text out of
  instruction positions; a model may still follow instructions inside the data it
  is shown. What following them can do is bounded by the grants.
- **Business sense.** The broker checks limits, not whether an action makes sense.
- **Covert channels.** E007 covers data flow and the decision to call; timing and
  cost are not modelled.

## 2. By design: what is trusted

From [GUARANTEES.md](../GUARANTEES.md) section 2 and [SECURITY.md](../../SECURITY.md):

- The operator: run configs, manifests, grants, and the claims it makes about
  services (that a service deduplicates by idempotency key). A false claim can lead
  a recovery to resend an action.
- An app's own code computing grants, app inputs and scopes.
- Library capabilities (`@amount`, `canonical`): a library that misreports an amount
  or maps two targets to one form defeats the limits that depend on it.
- C code granted through `cap ffi`: it can do anything the process can. Such runs are
  marked unchecked. On macOS a C library is loaded by path after it is hashed, so a
  file replaced in that moment would not be noticed (Linux loads the hashed
  descriptor). Libraries it depends on are not pinned.
- Whoever can write the journal directory, or both `staal.lock` and
  `.staal/packages/`.
- The machine: a disk that acknowledges `fsync` without writing breaks durability.
- The OAuth authorization server the operator names, as an issuer of tokens.

## 3. The MCP server over HTTP

From [MCP.md](../MCP.md) sections 12.5 and 12.9:

- **Not in a release build** until the independent review.
- **No token revocation.** A token is accepted until it expires; there is no
  introspection (RFC 7662). Revoked signing keys stay usable for at most 24 hours if
  the authorization server cannot be reached.
- **Strict token profile.** Authorization servers that do not set `typ: at+jwt`, or
  that issue opaque tokens, cannot be used.
- **Scopes are exact strings** with no hierarchy.
- **Requests without a valid token** are bounded by the connection limit and the
  time limits, not per client: an attacker can use connections up to
  `max_connections` (each for at most the header time limit).
- **A client that does not read its answer** loses it at shutdown after 10 s.
- **Not supported:** opaque tokens, mutual TLS, HTTP/2, the 2025-11-25 protocol over
  HTTP, `subscriptions/listen`, grants computed per principal other than through
  separate tools and claim-sourced app inputs.

## 4. Durability and time

- **A request a listening program accepted is lost if the process crashes** before
  it answers; a connection cannot outlive its process. The runtime never answers
  twice and says when a request was lost (RUNTIME.md section 12).
- **A clock jump across a restart** can move quota windows and `once` key expiry;
  the host warns about it. While a host runs, it reads the clock as at most a minute
  ahead of the time since boot.
- **C code cannot be interrupted**; a tool granted C functions is warned about at
  start.
- **Without `wait_ms`, an accept waits for ever** (refused in a tool).

## 5. Platforms

- Linux (x86_64, arm64) and macOS (Apple Silicon, Intel) only. Windows is not
  supported.
- SQLite actions and `staalc build` executables: Linux only.
- File actions and listening with TLS: Linux and macOS only (the checks they need
  are Unix ones).

## 6. Assurance

- **Tested, not proven.** The guarantees are checked by tests, crash injection at
  every journal point on both engines, deliberate faults and fuzzing; nothing is
  formally verified, including the checker's soundness.
- **No independent review yet**; this package prepares the first.
- **Live services** (Claude, Stripe, Termii, Resend, Slack) are tested against local
  stand-ins built from their documentation, not with real keys.
- **No long-running or load tests** (hours of running, many connections, large
  journals).
- **Release archives are not signed** and carry no build provenance (SUPPLY_CHAIN.md
  section 6).
- **One maintainer.** SECURITY.md states the response times this allows.
