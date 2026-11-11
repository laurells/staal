// `agent.described.json` (AGENT_MANIFEST.md section 5): what `staalc` computes from an
// agent's checked program and its locked packages, written by `staalc agent describe`
// and compared byte for byte by `staalc agent check`. Its form is
// docs/schemas/agent.described.schema.json.
//
// It is serialized in the JSON Canonicalization Scheme (RFC 8785, `crate::jcs`), and
// every list whose order carries no meaning is sorted by a stated key, so two `staalc`
// builds of one version on any platform write the same bytes:
//
//   dependencies   by name, each package's files by path
//   tools          by name
//   grants         by name
//   inputs         by name
//   consumers      by grant, then holder (crate::consumers)
//   failure        by grant
//   network        by grant
//   packages       by name (package_boundaries), what each receives as `consumers`
//   expenditure    by currency
//
// Every value here is computed by the code that enforces it: the fingerprint is the
// one a journal binds (`identity`), the interface and grants are `staalc schema`'s,
// consumers are `crate::consumers`', model use is the compiler's budget analysis
// (`budget::worst_cases`), and the expenditure ceiling follows the formula the tool
// host reserves a quota with (MCP.md section 6).

use super::{Checked, GrantNode, ToolDef};
use crate::ast::{Program, Ty};
use crate::interp::money::{Currency, Money};
use serde_json::{json, Map, Value as Json};
use std::collections::BTreeMap;

/// The version of the described form, which is that of the specification.
pub const DESCRIBED_VERSION: &str = super::MANIFEST_VERSION;

fn bound(b: crate::budget::Bound) -> Json {
    match b {
        crate::budget::Bound::Fin(n) => json!(n),
        crate::budget::Bound::Unbounded => json!("unbounded"),
    }
}

/// What a run does with a request through a runtime capability left in doubt by a
/// crash (RUNTIME.md section 11): `repeat` it (safe to repeat), `resend_with_key` it
/// (the service deduplicates by the idempotency key the runtime sends), or stop for an
/// `operator` to answer.
fn recovery(cap: &str, idempotency_key: bool) -> &'static str {
    const REPEATABLE: [&str; 9] = [
        "net.http.get",
        "fs.read",
        "fs.write",
        "db.sqlite.query",
        "db.postgres.query",
        "net.tcp.accept",
        "net.tcp.reply",
        "net.tls.accept",
        "net.tls.reply",
    ];
    if REPEATABLE.contains(&cap) {
        "repeat"
    } else if idempotency_key {
        "resend_with_key"
    } else {
        "operator"
    }
}

/// A runtime capability among a tool's grants and their needs.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct RuntimeGrant {
    grant: String,
    cap: String,
    idempotency_key: bool,
    /// The `only` its type writes, if any.
    program_only: Option<Vec<String>>,
    /// The `reach` agent.json declares, if any, placeholders kept.
    reach: Option<Vec<String>>,
    /// The private addresses agent.json declares for it, in canonical text, and whether
    /// it may reach cloud metadata services.
    private: Vec<String>,
    cloud_metadata: bool,
}

/// Every runtime capability among a tool's grants and their needs, from the program's
/// types and agent.json.
fn runtime_grants(prog: &Program, grant: &str, cap: &str, only: &Option<Vec<String>>, node: &GrantNode, out: &mut Vec<RuntimeGrant>) {
    if crate::interp::is_runtime_cap(cap) {
        out.push(RuntimeGrant {
            grant: grant.to_string(),
            cap: cap.to_string(),
            idempotency_key: node.idempotency_key == Some(true),
            program_only: only.clone(),
            reach: node.reach.clone(),
            private: node.addresses.iter().flat_map(|p| p.private.iter().map(|n| n.text())).collect(),
            cloud_metadata: node.addresses.as_ref().is_some_and(|p| p.cloud_metadata),
        });
        return;
    }
    let Some(decl) = prog.caps.iter().find(|c| c.actions.iter().any(|a| c.action_path(&a.name) == cap)) else { return };
    for (n, t, _) in &decl.needs {
        if let (Ty::Cap { path, limits }, Some(child)) = (t, node.needs.get(n)) {
            runtime_grants(prog, &format!("{grant}.{n}"), &path.join("."), &limits.only, child, out);
        }
    }
}

/// Whether requests through `cap` leave the machine: the network, and databases on
/// other machines.
/// The addresses a network grant reaches: for a capability that connects to addresses a
/// program chooses, public ones and the private ones declared, checked when it
/// connects; for any other (a listener, a database the operator configures), null.
fn addresses(g: &RuntimeGrant) -> Json {
    if !crate::interp::connects(&g.cap) {
        return Json::Null;
    }
    json!({ "public": true, "private": g.private, "cloud_metadata": g.cloud_metadata })
}

fn networked(cap: &str) -> bool {
    cap.starts_with("net.") || cap.starts_with("db.postgres.")
}

/// When a network capability's connections are verified with TLS against the host
/// name: `https_targets` (net.http: every https URL, and only loopback hosts may use
/// plain http), `always` (net.tls.*; db.postgres verifies when its grant asks for TLS,
/// which the business sets at install, so `per_grant`), or `never` (net.tcp.*).
fn tls(cap: &str) -> &'static str {
    if cap.starts_with("net.http.") {
        "https_targets"
    } else if cap.starts_with("net.tls.") {
        "always"
    } else if cap.starts_with("db.postgres.") {
        "per_grant"
    } else {
        "never"
    }
}

/// The external expenditure ceiling of one run, per currency: over the entry's
/// money-limited capabilities, `max`, or `each` times `uses` where there is no `max`;
/// unbounded in a currency where a capability has `each` and neither `uses` nor `max`.
fn expenditure(prog: &Program, entry: &str) -> Result<Json, String> {
    let f = prog.fns.iter().find(|f| f.name == entry).ok_or_else(|| format!("no function '{entry}'"))?;
    // Per currency: the ceiling in minor units, or None for unbounded.
    let mut per: BTreeMap<&'static str, (Currency, Option<i64>)> = BTreeMap::new();
    for p in &f.params {
        let Ty::Cap { limits, .. } = &p.ty else { continue };
        let money = |l: &crate::ast::Limit| -> Result<Money, String> {
            let unit = l.unit.as_deref().ok_or_else(|| format!("parameter '{}': a money limit has no currency", p.name))?;
            Money::from_major(l.amount, Currency::parse(unit)?)
        };
        let max = limits.max.as_ref().map(money).transpose()?;
        let each = limits.each.as_ref().map(money).transpose()?;
        let (currency, most) = match (max, each, limits.uses) {
            (Some(m), _, _) => (m.currency, Some(m.minor)),
            (None, Some(e), Some(u)) => {
                let u = i64::try_from(u).map_err(|_| format!("parameter '{}': uses is too large", p.name))?;
                let minor = e.minor.checked_mul(u).ok_or_else(|| format!("parameter '{}': each times uses is too large", p.name))?;
                (e.currency, Some(minor))
            }
            (None, Some(e), None) => (e.currency, None),
            (None, None, _) => continue,
        };
        let slot = per.entry(currency.code).or_insert((currency, Some(0)));
        slot.1 = match (slot.1, most) {
            (Some(a), Some(b)) => Some(a.checked_add(b).ok_or_else(|| format!("the ceiling in {} is too large", currency.code))?),
            _ => None,
        };
    }
    Ok(Json::Array(
        per.into_iter()
            .map(|(code, (currency, minor))| match minor {
                Some(minor) => json!({ "currency": code, "amount": Money { minor, currency }.amount_string() }),
                None => json!({ "currency": code, "unbounded": true }),
            })
            .collect(),
    ))
}

fn tool(def: &ToolDef, checked: &crate::Checked, authority: &BTreeMap<String, Json>) -> Result<Json, String> {
    let prog = checked.result.as_ref().map_err(|_| format!("{} has check errors", def.program))?;
    let schema = crate::schema_of(checked, prog, Some(&def.entry))?;
    let f = prog.fns.iter().find(|f| f.name == def.entry).ok_or_else(|| format!("no function '{}'", def.entry))?;
    let mut inputs: Vec<Json> = Vec::new();
    for p in &f.params {
        if matches!(p.ty, Ty::Cap { .. }) || matches!(&p.ty, Ty::Named(x) if x == "Secret") {
            continue;
        }
        let provenance = if def.app_inputs.contains(&p.name) { "app" } else { "caller" };
        inputs.push(json!({ "name": p.name, "provenance": provenance }));
    }
    inputs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let mut grants: Vec<Json> = schema["grants"].as_array().cloned().unwrap_or_default();
    grants.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let mut runtime = Vec::new();
    for p in &f.params {
        if let (Ty::Cap { path, limits }, Some(node)) = (&p.ty, def.grants.get(&p.name)) {
            runtime_grants(prog, &p.name, &path.join("."), &limits.only, node, &mut runtime);
        }
    }
    runtime.sort();
    // After a crash, a request whose result was recorded is replayed, never made again,
    // and one recorded as not sent is a failure the program saw. A request left in
    // doubt (it may or may not have reached the service) is handled by `recovery`; with
    // `operator`, the run stops and its outcome stays unknown until an operator answers.
    let failure: Vec<Json> = runtime
        .iter()
        .map(|g| {
            let r = recovery(&g.cap, g.idempotency_key);
            json!({ "grant": g.grant, "cap": g.cap, "recovery": r, "relies_on_service": r == "resend_with_key",
                    "outcome_can_be_unknown": r == "operator" })
        })
        .collect();
    // What a network grant's reach means: the canonical target of every request is
    // checked against it (host, port, path and query; no redirect followed), TLS as
    // `tls` says; where a host name resolves is the machine's DNS, trusted for public
    // addresses. For a capability that connects to addresses a program chooses, the
    // address connected to is checked too: public ones, and the private ones declared
    // (AGENT_MANIFEST.md section 5, "Network reach"; RUNTIME.md section 12, "Addresses").
    let network: Vec<Json> = runtime
        .iter()
        .filter(|g| networked(&g.cap))
        .map(|g| {
            json!({ "grant": g.grant, "cap": g.cap, "reach": g.reach, "program_only": g.program_only,
                    "checked": "canonical_target", "redirects_followed": false, "tls": tls(&g.cap),
                    "dns_trusted": true, "addresses": addresses(g) })
        })
        .collect();
    // For each package of the project, its declared authority and what this tool can
    // hand it: the package consumers of `consumers`, each with the boundary its slot
    // imposes. A package receives nothing but these.
    let package_boundaries: Vec<Json> = authority
        .iter()
        .map(|(name, declared)| {
            let receives: Vec<Json> = schema["consumers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["holder"]["kind"] == "package" && c["holder"]["package"] == name.as_str())
                .map(|c| json!({ "grant": c["grant"], "cap": c["cap"], "function": c["holder"]["function"],
                                  "parameter": c["holder"]["parameter"], "limits": c["limits"] }))
                .collect();
            json!({ "package": name, "declared_authority": declared, "receives": receives })
        })
        .collect();
    let cost = checked.costs.get(&def.entry).ok_or_else(|| format!("no cost for '{}'", def.entry))?;
    // C functions a run can be granted: directly, or through a library capability's needs.
    let c_functions = schema["consumers"].as_array().is_some_and(|c| c.iter().any(|x| x["cap"].as_str().is_some_and(|p| p.starts_with("ffi."))));
    Ok(json!({
        "name": def.name,
        "program": def.program,
        "entry": def.entry,
        "fingerprint": crate::identity::LABEL,
        "program_sha256": crate::interp::journal::sha256_hex(checked.fingerprint.as_bytes()),
        "interface": { "input": schema["input"], "output": schema["output"], "secrets": schema["secrets"] },
        "grants": grants,
        "consumers": schema["consumers"],
        "consumer_analysis": schema["consumer_analysis"],
        "inputs": inputs,
        "model_use": { "calls": bound(cost.calls), "tokens": bound(cost.tokens) },
        "expenditure_ceiling": expenditure(prog, &def.entry)?,
        "failure": failure,
        "network": network,
        "package_boundaries": package_boundaries,
        "unchecked": { "c_functions": c_functions, "console": schema["reads_console"] },
    }))
}

/// The described form of a checked agent, as JSON.
pub fn describe(root: &std::path::Path, checked: &Checked) -> Result<Json, String> {
    let m = &checked.manifest;
    let mut dependencies = Vec::new();
    let mut authority_of: BTreeMap<String, Json> = BTreeMap::new();
    if let Some(project) = crate::packages::Project::open(root)? {
        for (name, p) in &project.packages {
            let mut authority = Map::new();
            for (cap, a) in &p.manifest.authority {
                authority.insert(
                    cap.clone(),
                    match a {
                        crate::packages::Authority::Any => json!("any"),
                        crate::packages::Authority::Only(list) => json!(list),
                    },
                );
            }
            authority_of.insert(name.clone(), Json::Object(authority.clone()));
            let files: Vec<Json> = p.files.iter().map(|(path, sha)| json!({ "path": path, "sha256": sha })).collect();
            dependencies.push(json!({
                "name": name,
                "git": p.dep.git,
                "commit": p.dep.commit,
                "version": p.manifest.version.as_ref().map(|v| v.to_string()),
                "files": files,
                "authority": authority,
            }));
        }
    }
    let mut tools: Vec<Json> = Vec::new();
    for (def, c) in m.tools.iter().zip(&checked.programs) {
        tools.push(tool(def, c, &authority_of).map_err(|e| format!("tool '{}': {e}", def.name))?);
    }
    tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(json!({
        "described": DESCRIBED_VERSION,
        "agent": m.agent,
        "version": m.version,
        "staalc": env!("CARGO_PKG_VERSION"),
        "dependencies": dependencies,
        "tools": tools,
    }))
}

/// The bytes of `agent.described.json`: the description in RFC 8785 form.
pub fn bytes(root: &std::path::Path, checked: &Checked) -> Result<String, String> {
    crate::jcs::canonical(&describe(root, checked)?)
}
