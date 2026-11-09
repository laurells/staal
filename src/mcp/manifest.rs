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
    /// Each tool's `scopes`, in the order of `tools`: the OAuth scopes a token must hold
    /// to call it (section 12.5). Empty for every tool unless `server.http.auth` is set.
    pub tool_scopes: Vec<Vec<String>>,
    /// Each tool's app inputs taken from token claims, as (input, claim), in the order of
    /// `tools` (section 12.1). Empty unless `server.http.auth` is set.
    pub tool_claims: Vec<Vec<(String, String)>>,
    /// Each tool's `per_principal.max_concurrent_runs`, in the order of `tools`
    /// (section 12.6). None unless `server.http.auth` is set and the tool gives one.
    pub tool_principal_runs: Vec<Option<usize>>,
    /// `server.http.per_principal.max_concurrent_runs` when tokens are checked: kept here
    /// because the HTTP transport takes `http` out of the manifest to serve it.
    pub principal_max_runs: Option<usize>,
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
    let oauth = matches!(http.as_ref().map(|h| &h.auth), Some(super::http::Auth::OAuth(_)));
    let mut tools: Vec<Tool> = Vec::new();
    let mut tool_scopes = Vec::new();
    let mut tool_claims = Vec::new();
    let mut tool_principal_runs = Vec::new();
    let mut warnings = Vec::new();
    for (i, t) in list.iter().enumerate() {
        let at = format!("tools[{i}]");
        // The keys only the HTTP transport reads are taken out before the host reads the rest.
        let mut t = t.clone();
        let scopes = match t.as_object_mut().and_then(|o| o.remove("scopes")) {
            None => Vec::new(),
            Some(_) if !oauth => return Err(format!("{at}.scopes: only a server with server.http.auth checks scopes")),
            Some(v) => {
                let list = v.as_array().filter(|l| !l.is_empty()).ok_or(format!("{at}.scopes: expected a list of OAuth scopes"))?;
                let mut out: Vec<String> = Vec::new();
                for (j, s) in list.iter().enumerate() {
                    let s = s.as_str().filter(|s| super::http::valid_scope_token(s)).ok_or(format!("{at}.scopes[{j}]: expected a scope (visible ASCII, no space, \" or \\)"))?;
                    if !out.iter().any(|x| x == s) {
                        out.push(s.to_string());
                    }
                }
                out
            }
        };
        // Limits per principal: the tool's own, and its share of the quota.
        let only_oauth = |k: &str| format!("{at}.{k}: only a server with server.http.auth knows its principals");
        let principal_runs = match t.as_object_mut().and_then(|o| o.remove("per_principal")) {
            None => None,
            Some(_) if !oauth => return Err(only_oauth("per_principal")),
            Some(v) => {
                let pat = format!("{at}.per_principal");
                let p = v.as_object().ok_or(format!("{pat}: expected an object"))?;
                tool::keys_only(p, &pat, &["max_concurrent_runs"])?;
                Some(tool::whole(p, "max_concurrent_runs", &pat, 1, 256)?.ok_or(format!("{pat}.max_concurrent_runs: missing"))? as usize)
            }
        };
        let share = match t.get_mut("quota").and_then(Json::as_object_mut).and_then(|q| q.remove("per_principal")) {
            None => None,
            Some(_) if !oauth => return Err(only_oauth("quota.per_principal")),
            Some(v) => {
                let sat = format!("{at}.quota.per_principal");
                let p = v.as_object().ok_or(format!("{sat}: expected an object"))?;
                tool::keys_only(p, &sat, &["calls", "money"])?;
                let calls = tool::whole(p, "calls", &sat, 1, u32::MAX as u64)?;
                let money = p.get("money").map(|m| tool::money(m, &format!("{sat}.money"))).transpose()?;
                if calls.is_none() && money.is_none() {
                    return Err(format!("{sat}: give \"calls\", \"money\" or both"));
                }
                Some(tool::Share { calls, money })
            }
        };
        // App inputs from claims: {"input": {"claim": "name"}}, only where tokens are checked.
        let mut claims: Vec<(String, String)> = Vec::new();
        if oauth && let Some(v) = t.get("app_inputs").cloned() {
            let o = v.as_object().filter(|o| !o.is_empty()).ok_or(format!(
                "{at}.app_inputs: expected an object naming, for each input, the token claim it comes from: {{\"customer\": {{\"claim\": \"sub\"}}}}"
            ))?;
            for (input, spec) in o {
                let claim = spec
                    .as_object()
                    .filter(|s| s.len() == 1)
                    .and_then(|s| s.get("claim"))
                    .and_then(Json::as_str)
                    .filter(|c| !c.is_empty())
                    .ok_or(format!("{at}.app_inputs.{input}: expected {{\"claim\": \"<a claim of the access token>\"}}"))?;
                claims.push((input.clone(), claim.to_string()));
            }
            t["app_inputs"] = Json::Array(claims.iter().map(|(i, _)| Json::String(i.clone())).collect());
        }
        // A tool whose inputs come from claims is defined as an app's is: the inputs are
        // the definer's, never the caller's.
        let definer = if claims.is_empty() { Definer::Manifest } else { Definer::App(tool::Grants::Fixed) };
        let mut tool = tool::parse_tool(&t, &at, dir, model, &settings, definer, &mut warnings)?;
        if let Some(share) = share {
            let sat = format!("{at}.quota.per_principal");
            let q = tool.quota.as_mut().ok_or(format!("{sat}: the tool has no quota"))?;
            match (share.calls, q.calls) {
                (Some(_), None) => return Err(format!("{sat}.calls: the tool's quota counts no calls")),
                (Some(s), Some(all)) if s > all => return Err(format!("{sat}.calls: more than the tool's quota of {all}")),
                _ => {}
            }
            match (share.money, q.money) {
                (Some(_), None) => return Err(format!("{sat}.money: the tool's quota counts no money")),
                (Some(s), Some(all)) if s.currency != all.currency => {
                    return Err(format!("{sat}.money: in {}, but the tool's quota is in {}", s.currency.code, all.currency.code))
                }
                (Some(s), Some(all)) if s.minor > all.minor => return Err(format!("{sat}.money: more than the tool's quota of {all}")),
                _ => {}
            }
            q.per_scope = Some(share);
        }
        if tools.iter().any(|x| x.name == tool.name) {
            return Err(format!("tools[{i}].name: '{}' is used by an earlier tool", tool.name));
        }
        tools.push(tool);
        tool_scopes.push(scopes);
        tool_claims.push(claims);
        tool_principal_runs.push(principal_runs);
    }
    let principal_max_runs = http.as_ref().filter(|_| oauth).map(|h| h.per_principal.max_concurrent_runs);
    Ok(Manifest { name, settings, tools, warnings, http, tool_scopes, tool_claims, tool_principal_runs, principal_max_runs })
}
