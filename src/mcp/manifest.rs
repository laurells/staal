// The manifest of `staalc mcp` (docs/MCP.md section 2): read once at start and checked
// in full before the server answers anything. The server's own keys are read here; the
// host settings and each tool are read by `crate::host`, with the same checks a tool
// defined through the C interface gets.

use crate::host::tool::{self, Grants, Settings, Tool};
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
    let settings = tool::parse_settings(server, "server", dir, &["name"])?;
    let model = top.get("model");
    let list = top.get("tools").and_then(Json::as_array).ok_or("manifest: \"tools\" must be a list")?;
    if list.is_empty() {
        return Err("manifest: \"tools\" is empty; a server needs at least one tool".into());
    }
    let mut tools: Vec<Tool> = Vec::new();
    let mut warnings = Vec::new();
    for (i, t) in list.iter().enumerate() {
        let tool = tool::parse_tool(t, &format!("tools[{i}]"), dir, model, &settings, Grants::Fixed, false, &mut warnings)?;
        if tools.iter().any(|x| x.name == tool.name) {
            return Err(format!("tools[{i}].name: '{}' is used by an earlier tool", tool.name));
        }
        tools.push(tool);
    }
    Ok(Manifest { name, settings, tools, warnings })
}
