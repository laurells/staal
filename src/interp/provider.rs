// Model providers (RUNTIME.md section 5). The runtime talks to models only through
// this interface. Each request is complete in itself: there is no session state.

use serde_json::{json, Value as Json};

/// One stateless model request.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    /// The instruction position. Only trusted text ever reaches it (checked by E006).
    pub instruction: String,
    /// The data position: values the model reads but must not take instructions from.
    pub data: Vec<Json>,
    /// What the output must look like.
    pub output_schema: Json,
    /// The most output tokens the model may produce.
    pub max_output_tokens: u64,
}

impl ModelRequest {
    /// The request as a JSON object, exactly as recorded in the execution log.
    pub fn to_json(&self) -> Json {
        json!({
            "instruction": self.instruction,
            "data": self.data,
            "output_schema": self.output_schema,
            "max_output_tokens": self.max_output_tokens,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ModelReply {
    /// The model's output, already parsed as JSON; or why there is none although the
    /// model ran (a refusal, output cut off at the token limit), so that the tokens it
    /// used are still charged. A provider must never invent JSON from output that is
    /// not JSON.
    pub output: Result<Json, String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Why a provider has no reply.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderError {
    /// A definite failure: asking again unchanged would fail the same way (a refused
    /// key, a malformed request). The program sees a `provider` failure.
    Failed(String),
    /// The provider could not be reached or was overloaded, even after retrying. The
    /// run stops and can be resumed; the call is made again then.
    Unavailable(String),
}

impl From<String> for ProviderError {
    fn from(e: String) -> Self {
        ProviderError::Failed(e)
    }
}

pub trait Provider {
    /// How many input tokens this request would use, without sending it.
    fn count_input_tokens(&mut self, req: &ModelRequest) -> Result<u64, ProviderError>;
    /// Sends the request and returns the reply.
    fn complete(&mut self, req: &ModelRequest) -> Result<ModelReply, ProviderError>;
    /// Called instead of `complete` when a resumed run takes a model call's reply from
    /// its journal. Real providers do nothing; a scripted provider moves to its next reply.
    fn skip(&mut self) {}
}

/// The provider a run config's `model` object names.
pub fn from_config(cfg: &Json) -> Result<Box<dyn Provider + Send>, String> {
    let obj = cfg.as_object().ok_or("model: expected an object")?;
    match obj.get("provider").and_then(Json::as_str) {
        Some("script") => Ok(Box::new(ScriptProvider::from_config(cfg)?)),
        Some("anthropic") => Ok(Box::new(super::anthropic::AnthropicProvider::from_config(cfg)?)),
        _ => Err("model.provider: expected \"anthropic\" or \"script\"".into()),
    }
}

/// A deterministic provider for tests: replies come from a fixed script, in order.
/// Input tokens are counted as one token per 4 bytes of the request's JSON (rounded up).
pub struct ScriptProvider {
    replies: Vec<ScriptReply>,
    next: usize,
}

#[derive(Debug, Clone)]
pub struct ScriptReply {
    pub output: Option<Json>,
    pub output_tokens: u64,
    /// Overrides the reported input tokens, to test a provider that misreports.
    pub input_tokens: Option<u64>,
    pub error: Option<String>,
    /// The provider cannot be reached: the run stops, and resuming asks again.
    pub unavailable: Option<String>,
}

impl ScriptProvider {
    /// Parses `{"provider": "script", "responses": [{"output": ..., "output_tokens": n}, ...]}`.
    pub fn from_config(cfg: &Json) -> Result<ScriptProvider, String> {
        let obj = cfg.as_object().ok_or("model: expected an object")?;
        for k in obj.keys() {
            if k != "provider" && k != "responses" {
                return Err(format!("model: unknown key '{k}'"));
            }
        }
        if obj.get("provider").and_then(Json::as_str) != Some("script") {
            return Err("model.provider: expected \"script\"".into());
        }
        let list = obj.get("responses").and_then(Json::as_array).ok_or("model.responses: expected a list")?;
        let mut replies = Vec::new();
        for (i, r) in list.iter().enumerate() {
            let at = format!("model.responses[{i}]");
            let r = r.as_object().ok_or(format!("{at}: expected an object"))?;
            for k in r.keys() {
                if !["output", "output_tokens", "input_tokens", "error", "unavailable"].contains(&k.as_str()) {
                    return Err(format!("{at}: unknown key '{k}'"));
                }
            }
            let tokens = |k: &str| -> Result<Option<u64>, String> {
                match r.get(k) {
                    None => Ok(None),
                    Some(v) => v.as_u64().map(Some).ok_or(format!("{at}.{k}: expected a whole number")),
                }
            };
            let text = |k: &str| -> Result<Option<String>, String> {
                match r.get(k) {
                    None => Ok(None),
                    Some(v) => Ok(Some(v.as_str().ok_or(format!("{at}.{k}: expected a string"))?.to_string())),
                }
            };
            let (error, unavailable) = (text("error")?, text("unavailable")?);
            let given = [r.get("output").is_some(), error.is_some(), unavailable.is_some()];
            if given.iter().filter(|g| **g).count() != 1 {
                return Err(format!("{at}: needs exactly one of \"output\", \"error\" and \"unavailable\""));
            }
            replies.push(ScriptReply {
                output: r.get("output").cloned(),
                output_tokens: tokens("output_tokens")?.unwrap_or(1),
                input_tokens: tokens("input_tokens")?,
                error,
                unavailable,
            });
        }
        Ok(ScriptProvider { replies, next: 0 })
    }

    fn count(req: &ModelRequest) -> u64 {
        let bytes = req.to_json().to_string().len() as u64;
        bytes.div_ceil(4)
    }
}

impl Provider for ScriptProvider {
    fn count_input_tokens(&mut self, req: &ModelRequest) -> Result<u64, ProviderError> {
        Ok(Self::count(req))
    }

    fn skip(&mut self) {
        self.next += 1;
    }

    fn complete(&mut self, req: &ModelRequest) -> Result<ModelReply, ProviderError> {
        let Some(r) = self.replies.get(self.next).cloned() else {
            return Err(ProviderError::Failed(format!("script has no reply for model call #{}", self.next + 1)));
        };
        self.next += 1;
        if let Some(e) = r.unavailable {
            return Err(ProviderError::Unavailable(e));
        }
        if let Some(e) = r.error {
            return Err(ProviderError::Failed(e));
        }
        Ok(ModelReply {
            output: Ok(r.output.unwrap_or(Json::Null)),
            input_tokens: r.input_tokens.unwrap_or_else(|| Self::count(req)),
            output_tokens: r.output_tokens,
        })
    }
}
