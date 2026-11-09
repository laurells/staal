// The manifest of `staalc mcp` (docs/MCP.md section 2): read once at start and checked
// in full before the server answers anything. The server's own keys are read here; the
// host settings and each tool are read by `crate::host`, with the same checks a tool
// defined through the C interface gets.

use crate::host::tool::{self, Definer, Settings, Tool};
use serde_json::Value as Json;
use std::path::Path;

#[derive(Debug)]
pub struct Manifest {
    /// Reported to clients as the server's name.
    pub name: String,
    pub settings: Settings,
    pub tools: Vec<Tool>,
    /// Warnings found while checking, for the server to log.
    pub warnings: Vec<String>,
    /// `server.http`: serve over HTTP rather than stdio (docs/MCP.md section 12).
    pub http: Option<super::http::Settings>,
}

/// Whether the manifest at `path` asks for HTTP (`server.http`), read before the
/// manifest is checked, so stdio can take its descriptors first. A manifest that cannot
/// be read is reported by the full check that follows.
pub fn wants_http(path: &Path) -> Result<bool, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(false) };
    let Ok(j) = serde_json::from_str::<Json>(&text) else { return Ok(false) };
    Ok(j.get("server").and_then(|s| s.get("http")).is_some())
}

/// Reads and checks the manifest at `path`. Any problem stops the server.
pub fn load(path: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let j: Json = serde_json::from_str(&text).map_err(|e| format!("{}: not valid JSON: {e}", path.display()))?;
    let real = std::fs::canonicalize(path).map_err(|e| format!("cannot resolve {}: {e}", path.display()))?;
    let dir = real.parent().unwrap_or(Path::new("/")).to_path_buf();
    parse(&j, &dir)
}

fn parse(j: &Json, dir: &Path) -> Result<Manifest, String> {
    let top = j.as_object().ok_or("manifest: expected a JSON object")?;
    tool::keys_only(top, "manifest", &["server", "model", "tools"])?;
    let server = top.get("server").and_then(Json::as_object).ok_or("manifest: \"server\" must be an object")?;
    let name = tool::text(server, "name", "server")?.ok_or("server.name: missing")?;
    let settings = tool::parse_settings(server, "server", dir, &["name", "http"])?;
    let http = server.get("http").map(|h| super::http::parse(h, "server.http")).transpose()?;
    let model = top.get("model");
    let list = top.get("tools").and_then(Json::as_array).ok_or("manifest: \"tools\" must be a list")?;
    if list.is_empty() {
        return Err("manifest: \"tools\" is empty; a server needs at least one tool".into());
    }
    let mut tools: Vec<Tool> = Vec::new();
    let mut warnings = Vec::new();
    for (i, t) in list.iter().enumerate() {
        let tool = tool::parse_tool(t, &format!("tools[{i}]"), dir, model, &settings, Definer::Manifest, &mut warnings)?;
        if tools.iter().any(|x| x.name == tool.name) {
            return Err(format!("tools[{i}].name: '{}' is used by an earlier tool", tool.name));
        }
        tools.push(tool);
    }
    Ok(Manifest { name, settings, tools, warnings, http })
}
