#!/usr/bin/env python3
"""Deliberate faults in the MCP HTTP transport, its token verification, key fetching,
principals and per-principal limits (docs/MCP.md section 12): each fault is put into
the source, the tests that should notice it are run, and the fault is taken out
again. A fault the tests do not notice ("SURVIVED") means a test is missing.

    python3 scripts/mutants_http.py            # every fault
    python3 scripts/mutants_http.py J1 H3      # some

Run it on a clean working tree and run nothing else with cargo meanwhile: it edits
files under src/ and always restores them (with a new modification time, so cargo
never keeps a build made with a fault). The results are in
docs/security-review/ASSURANCE.md section 4.
"""
import os
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Each a list of `cargo test` invocations; a fault is caught if any of them fails.
JWT = [["--lib", "mcp::jwt"]]
KEYS = [["--lib", "mcp::keys"], ["--test", "mcp_keys_test"]]
HTTP = [["--test", "mcp_http_test"]]

# id, description, [(file, text, replacement)], test arguments
FAULTS = [
    ("J1", "typ not checked", [("src/mcp/jwt.rs", 'if !matches!(typ.as_deref(),', 'if false && !matches!(typ.as_deref(),')], JWT),
    ("J2", "crit allowed", [("src/mcp/jwt.rs", 'if header.contains_key("crit") {', 'if false {')], JWT),
    ("J3", "a key's own alg not checked", [("src/mcp/jwt.rs", "if !alg.fits(&key.kind) || key.alg.is_some_and(|a| a != alg) {", "if !alg.fits(&key.kind) {")], JWT),
    ("J4", "RSA keys under 2048 bits allowed", [("src/mcp/jwt.rs", "&& bits(n) < 2048", "&& bits(n) < 2048 && false")], JWT),
    ("J5", "signature not checked", [("src/mcp/jwt.rs", "if ok.is_err() {", "if false && ok.is_err() {")], JWT),
    ("J6", "iss not checked", [("src/mcp/jwt.rs", "if iss != e.issuer {", "if false && iss != e.issuer {")], JWT),
    ("J7", "aud not checked", [("src/mcp/jwt.rs", "if !aud_ok {", "if false && !aud_ok {")], JWT),
    ("J8", "exp not checked", [("src/mcp/jwt.rs", "if now > exp + leeway {", "if false && now > exp + leeway {")], JWT),
    ("J9", "nbf not checked", [("src/mcp/jwt.rs", "&& now + leeway < nbf", "&& false && now + leeway < nbf")], JWT),
    ("J10", "iat in the future allowed", [("src/mcp/jwt.rs", "if iat > now + leeway {", "if false && iat > now + leeway {")], JWT),
    ("J11", "jti not required", [("src/mcp/jwt.rs", 'text("jti")?;', 'let _ = text("jti");')], JWT),
    ("J12", "client_id not required", [("src/mcp/jwt.rs", 'let client_id = text("client_id")?;', 'let client_id = text("client_id").unwrap_or_else(|_| String::new());')], JWT),
    ("J13", "sub not required", [("src/mcp/jwt.rs", 'let sub = text("sub")?;', 'let sub = text("sub").unwrap_or_else(|_| String::new());')], JWT),
    ("J14", "a key set with two keys of one kid allowed", [("src/mcp/jwt.rs", "if out.iter().any(|o| o.kid == key.kid) {", "if false && out.iter().any(|o| o.kid == key.kid) {")], JWT),
    ("J15", "a key set of any size read", [("src/mcp/jwt.rs", "if keys.len() > MAX_KEYS {", "if false && keys.len() > MAX_KEYS {")], JWT),
    ("K1", "redirects followed", [("src/mcp/keys.rs", ".max_redirects(0)", ".max_redirects(5)"), ("src/mcp/keys.rs", "if (300..400).contains(&status) {", "if false && (300..400).contains(&status) {")], KEYS),
    ("K2", "metadata issuer not compared", [("src/mcp/keys.rs", "if got != issuer {", "if false && got != issuer {")], KEYS),
    ("K3", "documents of any size read", [("src/mcp/keys.rs", ".limit(MAX_DOCUMENT_BYTES)", ".limit(MAX_DOCUMENT_BYTES * 64)")], KEYS),
    ("K4", "documents of any depth read", [("src/mcp/keys.rs", "if super::proto::nesting(&body, MAX_DEPTH) > MAX_DEPTH {", "if false {")], KEYS),
    ("K5", "an unknown kid fetches every time", [("src/mcp/keys.rs", "let due = h.unknown_fetch.is_none_or(", "let due = true || h.unknown_fetch.is_none_or(")], KEYS),
    ("K6", "keys used however old", [("src/mcp/keys.rs", "if now.saturating_duration_since(h.fetched) > MAX_KEY_AGE {", "if false {")], KEYS),
    ("H1", "Host not checked", [("src/mcp/http.rs", 'Ok(Some(_)) => return refusal(StatusCode::MISDIRECTED_REQUEST, proto::INVALID_REQUEST, "this server does not answer for that host"),', "Ok(Some(_)) => {}")], HTTP),
    ("H2", "Origin not checked", [("src/mcp/http.rs", "Ok(Some(o)) if s.allowed_origins.iter().any(|a| a == o) => {}", "Ok(Some(_)) => {}")], HTTP),
    ("H3", "a request served without a valid token", [("src/mcp/http.rs", "            Err(r) => return r,\n        },\n    };", "            Err(_) => None,\n        },\n    };")], HTTP),
    ("H4", "two Authorization headers allowed", [("src/mcp/http.rs", "if all.next().is_some() {\n            let mut r = whole(StatusCode::BAD_REQUEST", "if false && all.next().is_some() {\n            let mut r = whole(StatusCode::BAD_REQUEST")], HTTP),
    ("H5", "request rate not limited", [("src/mcp/http.rs", "state.over_rate(&v.principal_key.scope(&c.iss, &c.sub)) {", "state.over_rate(&v.principal_key.scope(&c.iss, &c.sub)).filter(|_| false) {")], HTTP),
    ("H6", "tool scopes not checked", [("src/mcp/http.rs", "if !need.iter().all(|sc| c.scopes.contains(sc)) {", "if false {")], HTTP),
    ("H7", "every subject one principal", [("src/mcp/http.rs", "Some(super::Caller { scope: v.principal_key.scope(&c.iss, &c.sub),", 'Some(super::Caller { scope: v.principal_key.scope(&c.iss, ""),')], HTTP),
    ("H8", "Content-Type not checked", [("src/mcp/http.rs", 'if t != "application/json" || !charset_ok {', "if false {")], HTTP),
    ("H9", "Accept not checked", [("src/mcp/http.rs", 'if !accepts(&accept, "application/json") || !accepts(&accept, "text/event-stream") {', "if false {")], HTTP),
    ("H10", "the body's protocol version not compared", [("src/mcp/http.rs", "if body_version != Some(version.as_str()) {", "if false {")], HTTP),
    ("H11", "Mcp-Method not compared", [("src/mcp/http.rs", 'Ok(Some(_)) => return mismatch("Header mismatch: Mcp-Method does not match the body\'s method".into()),', "Ok(Some(_)) => {}")], HTTP),
    ("H12", "Mcp-Name not compared", [("src/mcp/http.rs", 'Ok(_) => return mismatch("Header mismatch: Mcp-Name does not match the body\'s params.name".into()),', "Ok(_) => {}")], HTTP),
    ("H13", "bodies of any size read", [("src/mcp/http.rs", "const MAX_BODY_BYTES: usize = proto::MAX_LINE_BYTES;", "const MAX_BODY_BYTES: usize = proto::MAX_LINE_BYTES * 4;")], HTTP),
    ("H14", "a closed connection never cancels", [("src/mcp/http.rs", "        if self.armed {\n            self.ctl.cancelled", "        if false && self.armed {\n            self.ctl.cancelled")], HTTP),
    ("H15", "a cancelled call still reserves and runs", [("src/host/mod.rs", "            if ctl.cancelled.load(Ordering::SeqCst) {\n                return None;\n            }\n            if st.ledger.poisoned()", "            if false {\n                return None;\n            }\n            if st.ledger.poisoned()")], HTTP),
    ("H16", "offline_access allowed in scopes_supported", [("src/mcp/http.rs", 'if s == "offline_access" {', "if false {")], HTTP),
    ("H17", "a principal's calls in progress not limited", [("src/mcp/mod.rs", "if mine.len() >= all_limit || mine.iter().filter(|t| **t == i).count() >= tool_limit {", "if false {")], HTTP),
    ("H18", "a principal's share of a quota not checked", [("src/host/mod.rs", 'fits(&u, q.window_ms, share.calls, share.money, reserve, "this caller\'s quota")?;', 'let _ = fits(&u, q.window_ms, share.calls, share.money, reserve, "this caller\'s quota");')], HTTP),
]


def run(invocations):
    """None if the faulty code does not build (the fault is wrong, not caught);
    otherwise whether any of the tests failed."""
    t = time.time()
    quiet = {"cwd": ROOT, "stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL}
    for args in invocations:
        base = ["cargo", "test", "--features", "failpoints", *args]
        if subprocess.run([*base, "--no-run"], **quiet).returncode != 0:
            return None, time.time() - t
    for args in invocations:
        if subprocess.run(["cargo", "test", "--features", "failpoints", *args], **quiet).returncode != 0:
            return True, time.time() - t
    return False, time.time() - t


def main(wanted):
    faults = [f for f in FAULTS if not wanted or f[0] in wanted]
    # Every change must apply exactly once before anything is run.
    for fid, _, edits, _ in faults:
        for path, old, _ in edits:
            n = open(os.path.join(ROOT, path)).read().count(old)
            if n != 1:
                raise SystemExit(f"{fid}: the text to change occurs {n} times in {path}")
    # The tests must pass without a fault, or every fault would look caught.
    every = []
    for f in faults:
        every += [a for a in f[3] if a not in every]
    failed, _ = run(every)
    if failed is not False:
        raise SystemExit("the tests do not pass without a fault; fix that first")
    survived = 0
    broken = 0
    for fid, what, edits, args in faults:
        originals = {}
        try:
            for path, old, new in edits:
                full = os.path.join(ROOT, path)
                text = originals.get(full) or open(full).read()
                originals.setdefault(full, text)
                cur = open(full).read()
                if cur.count(old) != 1:
                    raise SystemExit(f"{fid}: the text to change occurs {cur.count(old)} times in {path}")
                with open(full, "w") as f:
                    f.write(cur.replace(old, new))
            failed, secs = run(args)
        finally:
            for full, text in originals.items():
                with open(full, "w") as f:
                    f.write(text)
                os.utime(full)
        verdict = "DID NOT BUILD" if failed is None else "caught" if failed else "SURVIVED"
        survived += failed is False
        broken += failed is None
        print(f"{fid}\t{verdict}\t{what}\t({secs:.0f} s)", flush=True)
    print(f"{len(faults) - survived - broken} of {len(faults)} faults caught; {survived} survived; {broken} did not build", flush=True)
    return 1 if survived or broken else 0


if __name__ == "__main__":
    sys.exit(main(set(sys.argv[1:])))
