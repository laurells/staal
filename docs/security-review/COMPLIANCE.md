# Compliance with the standards Staal implements

For each external standard the code implements, the requirements that bear on
security, where the code meets them, and the test that checks it. A requirement not
met is said so, with the reason. The specifications themselves are not reproduced;
section numbers refer to the documents named.

Status words: **Met** (implemented and tested), **Met, untested** (implemented, no
test isolates it), **Not applicable** (a part of the standard Staal has no role in,
with the reason), **Not met** (with the reason; each is also in
[LIMITATIONS.md](LIMITATIONS.md)).

Paths are under `src/` unless given; tests are in `tests/` unless given.

## 1. MCP 2026-07-28: Streamable HTTP transport

Implemented in `mcp/http.rs` (`handle`), specified in [MCP.md](../MCP.md) section
12.3. Served only by a test build (SCOPE.md).

| Requirement | Status | Where | Test |
| --- | --- | --- | --- |
| One endpoint; a client request is one POSTed JSON-RPC message | Met | `handle` | `mcp_http_test.rs` `requests_are_served_as_over_stdio` |
| Validate `Origin` to prevent DNS rebinding; refuse a disallowed one with 403 | Met | `handle`, `allowed_origins` (exact string match) | `each_check_refuses_with_its_own_answer_in_order` |
| A server should check `Host` against the names it serves | Met (421) | `handle`, `allowed_hosts`; an absolute-form request target is refused (400) | same |
| `Accept` must list `application/json` and `text/event-stream` | Met (406) | `accepts` (RFC 9110 media ranges and `q`) | same; unit test `accept_headers_are_read_as_rfc_9110_says` |
| Answer a request with `application/json` or an SSE stream | Met: JSON, or SSE when the request has a `progressToken` | `handle` | `a_call_that_asks_for_progress_is_answered_as_a_stream` |
| No server-to-client requests on a request's stream | Met: the server sends none | `handle` | — (nothing to send) |
| A notification is answered 202 with no body | Met | `handle` | `each_check_refuses_with_its_own_answer_in_order` |
| `MCP-Protocol-Version` on every request; an unsupported one refused with `-32022` and the supported versions | Met. A missing header is answered `-32020` before the body is read | `handle` | same (both cases) |
| The protocol version header equals `_meta["io.modelcontextprotocol/protocolVersion"]`; `Mcp-Method` equals `method`; for `tools/call`, `Mcp-Name` (base64 sentinel form decoded) equals `params.name`; else `-32020` | Met | `handle`, `decode_name` | same; unit test `names_are_decoded_from_the_base64_sentinel` |
| A closed connection is the client's cancellation | Met: before the reservation nothing runs; after it the run completes and is recorded | `CancelGuard` (drop sets the flag), `Session` | `a_connection_closed_before_the_reservation_does_nothing_and_after_it_refunds_once`; `a_stream_closed_after_the_run_began_leaves_the_run_to_finish` |
| Batches are not part of the protocol | Met (400, `-32600`) | `handle` | `each_check_refuses_with_its_own_answer_in_order` |
| No session: `Mcp-Session-Id` and `Last-Event-ID` ignored; none issued | Met | `handle` | `requests_are_served_as_over_stdio` (no session header in any answer) |
| `resources/read` and `prompts/get` (methods with `Mcp-Name`) | Not applicable: not served (`-32601`) | `handle` | `each_check_refuses_with_its_own_answer_in_order` |
| Interoperability with the official SDK clients | Met: Python and TypeScript SDK clients list and call tools over HTTPS with OAuth `client_credentials` | `tests/mcp_sdk/` | `mcp_sdk_test.rs` `the_official_python_and_typescript_clients_list_and_call_tools` (CI, every platform) |

Note for the reviewer: the specification text was read from its published revision
when the transport was written. This table was written from the implementation and
[MCP.md](../MCP.md); checking it against the specification is SCOPE.md question Q6.

## 2. MCP 2026-07-28: Authorization

The server is an OAuth 2.1 resource server only ([MCP.md](../MCP.md) 12.4). Client
registration, PKCE, the `resource` request parameter and RFC 9207 `iss` checking are
the client's and authorization server's, and none passes through staalc: **Not
applicable**.

| Requirement | Status | Where | Test |
| --- | --- | --- | --- |
| Serve protected resource metadata (RFC 9728) | Met, at the path-inserted and root well-known locations, GET without a token | `Verifier::start`, `handle` | `the_protected_resource_metadata_is_served_without_a_token` |
| 401 with `WWW-Authenticate: Bearer resource_metadata=...` (and `scope` when configured) for a request without a token | Met | `Verifier::challenge`, `unauthorized` | `requests_need_a_valid_token_and_tools_their_scopes` |
| Validate that the token was issued for this resource (audience) | Met: `aud` must equal `resource` or contain it | `jwt.rs` `verify` | jwt fixtures "another audience", "aud array without the resource" |
| Tokens only in the `Authorization` header, never the query string | Met: query and body never read; two `Authorization` headers refused | `Verifier::authorize` | `requests_need_a_valid_token_and_tools_their_scopes` |
| 403 `insufficient_scope` naming the scopes required | Met: all of a tool's scopes in one challenge | `handle` | same |
| Do not pass the token through to other services | Met: never given to a run, program, notify command or request; never written | `Verifier::authorize`, `Caller` | the token, key and subject checks over every file and log in `mcp_http_test.rs` |
| Never list `offline_access` in `scopes_supported` | Met | `parse_oauth` | `the_http_settings_are_checked_before_anything_is_served` |
| No unauthenticated HTTP | Met: a release build has no `"auth": "none"` | `parse` | `a_release_build_refuses_http_until_it_has_authentication` |

## 3. RFC 9068: JWT profile for OAuth 2.0 access tokens

`mcp/jwt.rs` `verify`; rules numbered as in [MCP.md](../MCP.md) 12.5. The test
corpus is 38 tokens made by an independent library (PyJWT, `scripts/jwt_fixtures.py`)
in `tests/fixtures/jwt/`, each with its expected verdict
(`tokens_made_by_an_independent_library_are_judged_as_expected`).

| Requirement (section) | Status | Where |
| --- | --- | --- |
| `typ` is `at+jwt` (or `application/at+jwt`); refuse others (4) | Met, no setting relaxes it | `verify` |
| Validate `iss` exactly against the expected issuer (4) | Met: byte-for-byte | `verify` |
| Validate `aud` names this resource (4) | Met | `verify` |
| Verify the signature with the issuer's published keys; `alg` not `none` (4) | Met: RS256, PS256, ES256, EdDSA only; signature by ring 0.17.14 | `verify`, `Alg` |
| Check `exp` (4) | Met, with `jwt_leeway_s` (0 to 300, default 60) | `verify` |
| `iat` present; `sub`, `client_id`, `jti` present (2.2) | Met: all required as stated | `verify` |
| `scope` is a space-separated string (2.2.3) | Met | `verify`, `handle` |
| Encrypted tokens (JWE) | Not applicable: refused (a token is exactly three parts) | `verify` |

## 4. RFC 7515 / 7517 / 7518 / 8037: JWS, JWK, algorithms, EdDSA

| Requirement | Status | Where | Test |
| --- | --- | --- | --- |
| Compact serialization, base64url without padding, strict | Met: padding, non-alphabet characters and extra parts refused | `b64u`, `verify` | jwt fixtures |
| `crit` must be understood or the JWS refused (RFC 7515 4.1.11) | Met: any `crit` refused | `verify` | fixture "crit" |
| Keys not taken from `jku`, `jwk`, `x5u`, `x5c` | Met: ignored; only `kid` into the fetched set | `verify` | fixtures "a key in the header (jwk)", "jku" |
| Key type fits the algorithm; RSA at least 2048 bits (RFC 7518 3.3) | Met | `Alg::fits`, `usable`, `bits` | `key_sets_are_read_strictly`, `bits_are_counted_from_the_top_byte` |
| Known answers | Met: RFC 7515 appendix A.3 (ES256) and RFC 8037 appendix A.4 (Ed25519) examples verify | — | `the_rfc_7515_es256_example_verifies`, `the_rfc_8037_ed25519_example_verifies` |
| Fuzzing | No token not signed by a fetched key verifies | — | fuzz target `jwt` ([ASSURANCE.md](ASSURANCE.md)) |

## 5. RFC 8414 / OpenID Connect Discovery: authorization server metadata

`mcp/keys.rs`.

| Requirement | Status | Where | Test |
| --- | --- | --- | --- |
| Metadata at the well-known URL built from the issuer (path-inserted) (8414 3) | Met: RFC 8414 locations, then OIDC | `metadata_urls`, `discover` | `metadata_is_looked_for_where_the_specification_says`; `discovery_tries_each_location_in_order` |
| `issuer` in the metadata identical to the issuer used (8414 3.3) | Met, else the server refuses to start | `discover` | `metadata_that_cannot_be_trusted_is_refused` |
| TLS, certificate verified (8414 6) | Met: HTTPS only, Mozilla roots plus configured roots, no redirects, 5 s / 10 s, 256 KiB, nesting 16 | `Fetcher` | `a_fetch_is_https_to_a_trusted_server_within_its_time` |
| A JWK set with a repeated `kid`, or no usable key | Refused (a failed fetch) | `fetch_keys`, `parse_jwks` | `key_sets_that_cannot_be_used_are_refused` |

## 6. RFC 9728: OAuth 2.0 protected resource metadata

| Requirement | Status | Where | Test |
| --- | --- | --- | --- |
| `resource` member equals the resource identifier (2, 3.3) | Met (from the manifest's `resource`) | `Verifier::start` | `the_protected_resource_metadata_is_served_without_a_token` |
| Served at `/.well-known/oauth-protected-resource` with the resource's path inserted (3) | Met, and at the root | same | same |
| `WWW-Authenticate` `resource_metadata` parameter (5.1) | Met | `challenge` | `requests_need_a_valid_token_and_tools_their_scopes` |

## 7. RFC 6750: bearer tokens

| Requirement | Status | Where | Test |
| --- | --- | --- | --- |
| `Authorization: Bearer`, scheme case-insensitive (2.1) | Met | `authorize` | same |
| Form and query parameters (2.2, 2.3) | Not supported, by choice (the MCP specification forbids the query string) | — | — |
| No `error` in the challenge for a request without credentials (3.1) | Met | `unauthorized` | same |
| `invalid_token` 401, `insufficient_scope` 403, `invalid_request` 400 (3.1) | Met | `authorize`, `handle` | same |
| The reason for a refusal not revealed beyond the error code | Met: reasons logged on the server only, rate-limited, without the token | `Verifier::log` | the token, key and subject checks over every file and log in `mcp_http_test.rs` |

## 8. RFC 8707: resource indicators

The client sends `resource`; the authorization server puts it in `aud`. The server's
part is the `aud` check (section 3). **Met** for that part; the rest **Not
applicable**.

## 9. HTTP: RFC 9110 and RFC 9112

| Requirement | Status | Where | Test |
| --- | --- | --- | --- |
| Message framing, including a request with both `Content-Length` and `Transfer-Encoding` (9112 6.1, 6.3) | Met by hyper 1.x | `connection` | `bodies_are_framed_by_hyper_and_bounded` (a smuggled second request is never served) |
| 405 carries `Allow` (9110 15.5.6) | Met | `handle` | `each_check_refuses_with_its_own_answer_in_order` |
| 429 with `Retry-After` (RFC 6585 4) | Met | `handle` | `a_principal_over_its_request_rate_waits_while_another_is_served` |
| Header field values limited to visible ASCII (stricter than 9110 5.5's field-value) and given once | Met for the headers the server reads | `one_header` (`HeaderValue::to_str`) | `each_check_refuses_with_its_own_answer_in_order` |
| Slow-client limits | Met: headers 10 s, body 30 s (408), 64 KiB of headers, TLS handshake 10 s, idle 10 s | `connection`, `times` | `bodies_are_framed_by_hyper_and_bounded` |

## 10. TLS

| Item | Status | Where |
| --- | --- | --- |
| rustls 0.23 with ring; TLS 1.2 and 1.3 only; no renegotiation, no compression (rustls offers neither) | Met | `interp/tcp.rs` `server_config`; `interp/http.rs` |
| Server certificate verification for outgoing connections: Mozilla's roots (`webpki-root-certs`), host name, validity | Met | `interp/http.rs`, `mcp/keys.rs` |
| Listening: certificate within its validity period, private key matches it, key file mode 600 or 400 and owned by the user | Met | `tcp.rs` `check_key_file`, `check_certificate_names` |

Tests: `https_test.rs` (11), `tls_test.rs`, the TLS tests in `serve_test.rs` and
`tcp.rs`.

## 11. OWASP ASVS 5.0.0

The level to verify against is the reviewer's and owner's choice
([SCOPE.md](SCOPE.md)). This maps each ASVS chapter to where Staal answers it, so a
reviewer can go from a chapter to the code. ASVS is written for web applications;
chapters that do not fit a compiler and runtime are marked so rather than stretched.

| Chapter | Where in Staal | Fit |
| --- | --- | --- |
| V1 Encoding and Sanitization | E006 (no untrusted text in an instruction or SQL); SQL bound parameters; header checks (T12); refusal text escaped (T19) | applies |
| V2 Validation and Business Logic | inputs checked against the entry function's types before a run; `validate` (E001); broker limits (T9) | applies |
| V3 Web Frontend Security | — | not applicable: no browser front end |
| V4 API and Web Service | MCP over HTTP (sections 1, 9 above) | applies (HTTP transport) |
| V5 File Handling | `fs` capability (T14); packages (T56, T57) | applies |
| V6 Authentication | not an authentication server; token verification is V9 and V10 | partly |
| V7 Session Management | no sessions (stateless transport) | not applicable |
| V8 Authorization | capabilities and the broker (T2, T9, T10); tool scopes (T45); per-principal scope (T46) | applies |
| V9 Self-contained Tokens | RFC 9068 verification (section 3) | applies |
| V10 OAuth and OIDC | resource server parts (sections 2, 5 to 8) | applies (resource server only) |
| V11 Cryptography | SHA-256 (packages, journals), HMAC-SHA-256 (principal scope), signatures (ring); no custom primitives | applies |
| V12 Secure Communication | TLS (section 10) | applies |
| V13 Configuration | run config and manifest validated before anything runs; secrets only from the environment; release build refuses test-only settings | applies |
| V14 Data Protection | secrets kept out of journals, logs and models (T26); subject stored only as an HMAC (T46) | applies |
| V15 Secure Coding and Architecture | [UNSAFE.md](UNSAFE.md); [SUPPLY_CHAIN.md](SUPPLY_CHAIN.md); concurrency (T11, T28) | applies |
| V16 Security Logging and Error Handling | run logs and journals; refusals logged without secrets or tokens | applies |
| V17 WebRTC | — | not applicable |
