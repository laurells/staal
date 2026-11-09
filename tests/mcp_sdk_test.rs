// The official MCP SDK clients against `staalc mcp` (docs/MCP.md section 9): the
// Python SDK (tests/mcp_sdk/python_client.py) and the TypeScript SDK
// (tests/mcp_sdk/ts_client.mjs) start the server, list its tools and call them, under
// 2025-11-25 (initialize), under 2026-07-28 (pinned) and with the SDKs' own probe.
//
// The SDKs are not part of the build, so this test runs them only when
// STAAL_MCP_SDK=1, which CI sets after installing them: `pip install -r
// tests/mcp_sdk/requirements.txt` (into the Python STAAL_MCP_PYTHON names, default
// python3) and `npm ci` in tests/mcp_sdk.

#![cfg(unix)]

mod common;

use common::mcp::{case_dir, write_manifest, BIN};
use serde_json::json;
use std::path::Path;
use std::process::Command;

#[test]
fn the_official_python_and_typescript_clients_list_and_call_tools() {
    if std::env::var("STAAL_MCP_SDK").as_deref() != Ok("1") {
        eprintln!("skipped: set STAAL_MCP_SDK=1 with the SDKs installed (see this file's header)");
        return;
    }
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mcp_sdk");
    let python = std::env::var("STAAL_MCP_PYTHON").unwrap_or_else(|_| "python3".into());
    let clients: [(&str, Vec<String>); 2] = [
        (python.as_str(), vec![sdk.join("python_client.py").to_string_lossy().into_owned()]),
        ("node", vec![sdk.join("ts_client.mjs").to_string_lossy().into_owned()]),
    ];
    for (program, script) in &clients {
        for mode in ["legacy", "2026-07-28", "auto"] {
            let name = format!("sdk-{}-{mode}", if *program == "node" { "ts" } else { "py" });
            let dir = case_dir(&name);
            std::fs::write(
                dir.join("refund.staal"),
                "fn main(order: Text, amount: Money, pay: cap payments.refund(each: 500 USD)) -> Text {\n    let receipt = pay.refund(order, amount)\n    return \"Refunded \" + to_text(amount) + \" for order \" + order\n}\n",
            )
            .unwrap();
            let m = write_manifest(&dir, json!({ "server": { "name": "shop" }, "tools": [{
                "name": "refund_order", "description": "Refund part of an order.", "program": "refund.staal",
                "grants": { "pay": { "cap": "payments.refund", "each": { "amount": "50.00", "currency": "USD" }, "uses": 1 } }
            }] }));
            let out = Command::new(program).args(script).arg(BIN).arg(&m).arg(mode).current_dir(&sdk).output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.trim_end().ends_with("ok"),
                "{name}: {stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

/// The release gate of docs/MCP.md section 12.7: the official Python and TypeScript SDK
/// clients over Streamable HTTP with OAuth, against a TLS server and a fake
/// authorization server with a token endpoint. Each SDK finds the authorization server
/// from the 401 and the protected resource metadata, gets a token with the
/// client_credentials grant, and lists and calls tools, probing (`auto`) and pinned to
/// 2026-07-28.
#[test]
#[cfg(feature = "failpoints")]
fn the_official_clients_sign_in_and_call_tools_over_http() {
    use common::authserver::{AuthServer, Route};
    use std::io::Read;
    if std::env::var("STAAL_MCP_SDK").as_deref() != Ok("1") {
        eprintln!("skipped: set STAAL_MCP_SDK=1 with the SDKs installed (see this file's header)");
        return;
    }
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mcp_sdk");
    let python = std::env::var("STAAL_MCP_PYTHON").unwrap_or_else(|_| "python3".into());
    let ca = common::tls::Ca::new("SDK Test CA");
    let auth = AuthServer::start(ca.server(&["localhost"], None));
    let issuer = common::tokens::Issuer::new("sdk-key");

    let clients: [(&str, &str); 2] = [("py", "python_http_client.py"), ("ts", "ts_http_client.mjs")];
    for (lang, script) in clients {
        for mode in ["auto", "2026-07-28"] {
            let name = format!("sdk-http-{lang}-{mode}");
            let dir = case_dir(&name);
            let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let resource = format!("https://localhost:{port}/mcp");
            // What the authorization server answers: its metadata, its keys, and a token for
            // this server signed by its key.
            let token = issuer.token(&common::tokens::claims(&auth.base, &resource, "sdk-user", "tools refunds:write", &json!({})), &json!({}));
            auth.set("/.well-known/oauth-authorization-server", Route::json(&json!({
                "issuer": auth.base, "jwks_uri": format!("{}/jwks", auth.base),
                "authorization_endpoint": format!("{}/authorize", auth.base), "token_endpoint": format!("{}/token", auth.base),
                "response_types_supported": ["code"], "grant_types_supported": ["client_credentials"],
                "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"]
            })));
            auth.set("/jwks", Route::json(&json!({ "keys": [issuer.jwk()] })));
            auth.set("/token", Route::json(&json!({ "access_token": token, "token_type": "Bearer", "expires_in": 600, "scope": "tools refunds:write" })));
            let token_requests = auth.hits("/token");

            std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
            let (chain, key) = ca.listen_files(&dir, &["localhost"], None);
            std::fs::write(
                dir.join("refund.staal"),
                "fn main(order: Text, amount: Money, pay: cap payments.refund(each: 500 USD)) -> Text {\n    let receipt = pay.refund(order, amount)\n    return \"Refunded \" + to_text(amount) + \" for order \" + order\n}\n",
            )
            .unwrap();
            let m = write_manifest(&dir, json!({
                "server": { "name": "shop", "http": {
                    "listen": format!("127.0.0.1:{port}"), "resource": resource,
                    "tls": { "certificate_chain": chain, "private_key": key },
                    "auth": { "issuer": auth.base, "scopes_supported": ["tools"], "extra_root_certificates": [dir.join("ca.pem")],
                              "principal_key": { "env": "STAAL_SDK_PRINCIPAL_KEY" } } } },
                "tools": [{
                    "name": "refund_order", "description": "Refund part of an order.", "program": "refund.staal", "scopes": ["refunds:write"],
                    "grants": { "pay": { "cap": "payments.refund", "each": { "amount": "50.00", "currency": "USD" }, "uses": 1 } }
                }]
            }));
            let mut server = Command::new(BIN)
                .arg("mcp")
                .arg(&m)
                .env("STAAL_SDK_PRINCIPAL_KEY", "sdk-test-principal-key-of-32-bytes-or-more")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            // Ready once it accepts connections.
            let start = std::time::Instant::now();
            while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
                if let Ok(Some(st)) = server.try_wait() {
                    let mut err = String::new();
                    let _ = server.stderr.take().unwrap().read_to_string(&mut err);
                    panic!("{name}: the server stopped ({st}): {err}");
                }
                assert!(start.elapsed() < std::time::Duration::from_secs(30), "{name}: the server never listened");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }

            let ca_path = dir.join("ca.pem");
            let out = if lang == "py" {
                Command::new(&python).arg(sdk.join(script)).arg(&resource).arg(&ca_path).arg(&auth.base).arg(mode).current_dir(&sdk).output().unwrap()
            } else {
                Command::new("node").arg(sdk.join(script)).arg(&resource).arg(&auth.base).arg(mode).env("NODE_EXTRA_CA_CERTS", &ca_path).current_dir(&sdk).output().unwrap()
            };
            // SAFETY: kill with a valid signal on our own child's pid.
            unsafe { libc::kill(server.id() as i32, libc::SIGTERM) };
            let status = server.wait().unwrap();
            let mut err = String::new();
            let _ = server.stderr.take().unwrap().read_to_string(&mut err);
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.trim_end().ends_with("ok"),
                "{name}: {stdout}\n{}\nserver:\n{err}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(auth.hits("/token") > token_requests, "{name}: the SDK got its token from the token endpoint");
            assert_eq!(status.code(), Some(0), "{name}: {err}");
            assert!(err.contains("called by principal"), "{name}: the calls were made as the token's principal: {err}");
        }
    }
}
