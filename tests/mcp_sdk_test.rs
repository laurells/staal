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
