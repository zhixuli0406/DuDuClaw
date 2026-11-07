//! Fixed-origin public search with account selection and checked E parsing.
use std::{future::Future, io::{Cursor, Read}, pin::Pin, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use duduclaw_isolation::{ids::EmployeeId, worker::{ContentFormat, ParseOperation, ParseOptions, ParseResult, ParsedItem}};
use duduclaw_mcp_boundary::{tool_policy::AuthorizedToolCall, RequestContext, VerificationError};
use serde_json::{Value, json};
use crate::isolation_connectors::{BoundConnectorExecutor, LiveConnectorSelectionPolicy, ProductConnectorAccount, SelectedConnector};

pub struct SearchConnectorProvider {
    employee: EmployeeId,
    account: ProductConnectorAccount,
    http: reqwest::Client,
}
impl SearchConnectorProvider {
    /// InstalledWorkerServices can only be obtained after installing a checked
    /// launcher; absence is a startup error, never a local HTML parser fallback.
    #[cfg(feature = "isolation-worker-integration")]
    pub fn new(employee: EmployeeId, account: ProductConnectorAccount,
        _worker: &crate::isolation_execution::InstalledWorkerServices) -> Result<Self, VerificationError> {
        if !matches!(account, ProductConnectorAccount::SharedHome { .. }) || account.account().is_empty() {
            return Err(VerificationError::Invalid);
        }
        let http = search_http_client()?;
        Ok(Self { employee, account, http })
    }
    async fn run(&self, selected: SelectedConnector, call: AuthorizedToolCall) -> Result<Value, VerificationError> {
        selected.verify_call(&call)?;
        if selected.account() != self.account.account() || call.name() != "web_search" { return Err(VerificationError::BindingMismatch); }
        let query = call.arguments().get("query").and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 65_536).ok_or(VerificationError::Invalid)?;
        let selected = Arc::new(selected);
        let invocation = Invocation::new();
        let url = search_url(query)?;
        let request = self.http.get(url).header("User-Agent", "DuDuClaw/0.6");
        selected.recheck()?;
        let mut response = request.send().await.map_err(|_| VerificationError::Unavailable)?;
        if !response.status().is_success() { return Err(VerificationError::Unavailable); }
        selected.recheck()?;
        let limit = duduclaw_parse_worker::policy::MAX_FILE_BYTES as usize;
        if response.content_length().is_some_and(|length| length > limit as u64) { return Err(VerificationError::Invalid); }
        let mut bytes = Vec::new();
        loop {
            selected.recheck()?;
            let chunk = response.chunk().await.map_err(|_| VerificationError::Unavailable)?;
            let Some(chunk) = chunk else { break; };
            if chunk.len() > limit.saturating_sub(bytes.len()) { return Err(VerificationError::Invalid); }
            bytes.extend_from_slice(&chunk);
        }
        selected.recheck()?;
        let input = SearchInput { bytes: Cursor::new(bytes), selected: selected.clone(), active: invocation.active.clone() };
        let result = tokio::task::spawn_blocking(move || parse(input)).await.map_err(|_| VerificationError::Unavailable)??;
        selected.recheck()?;
        Ok(json!({"content":[{"type":"text","text":result}]}))
    }
}
pub(crate) fn search_http_client() -> Result<reqwest::Client, VerificationError> {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).no_proxy().retry(reqwest::retry::never())
        .timeout(std::time::Duration::from_secs(10)).build().map_err(|_| VerificationError::Unavailable)
}
fn search_url(query: &str) -> Result<url::Url, VerificationError> {
    let mut url = url::Url::parse("https://html.duckduckgo.com/html/").map_err(|_| VerificationError::Invalid)?;
    url.query_pairs_mut().append_pair("q", query);
    Ok(url)
}
struct Invocation { active: Arc<AtomicBool> }
impl Invocation { fn new() -> Self { Self { active: Arc::new(AtomicBool::new(true)) } } }
impl Drop for Invocation { fn drop(&mut self) { self.active.store(false, Ordering::SeqCst); } }
struct SearchInput { bytes: Cursor<Vec<u8>>, selected: Arc<SelectedConnector>, active: Arc<AtomicBool> }
impl Read for SearchInput {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        verify_input(self).map_err(|_| std::io::Error::other("search authority revoked"))?;
        self.bytes.read(buffer)
    }
}
fn verify_input(input: &SearchInput) -> Result<(), duduclaw_parse_worker::worker::ParseError> {
    if !input.active.load(Ordering::SeqCst) { return Err(duduclaw_parse_worker::worker::ParseError::Unavailable); }
    input.selected.recheck().map_err(|_| duduclaw_parse_worker::worker::ParseError::Unavailable)
}
fn parse(mut input: SearchInput) -> Result<String, VerificationError> {
    verify_input(&input).map_err(|_| VerificationError::Unavailable)?;
    let mut request = duduclaw_parse_worker::client::request(ParseOperation::ExtractHtml, ContentFormat::Text, 0);
    request.options = Some(ParseOptions::SearchHtml);
    let (response, outputs) = duduclaw_parse_worker::client::invoke_output_reader_checked(&request, &mut input, verify_input)
        .map_err(|_| VerificationError::Unavailable)?;
    if !outputs.is_empty() { return Err(VerificationError::Invalid); }
    let ParseResult::Complete { items } = response.result else { return Err(VerificationError::Unavailable); };
    let result = format_search_items(items.as_slice())?;
    verify_input(&input).map_err(|_| VerificationError::Unavailable)?;
    Ok(result)
}
pub(crate) fn format_search_items(items: &[ParsedItem]) -> Result<String, VerificationError> {
    let mut lines = Vec::new();
    // SearchHtml emits at most five snippets. Enforce the same output cap here
    // instead of failing the whole search if a worker returns additional items.
    for (index, item) in items.iter().take(5).enumerate() {
        let ParsedItem::Html { input_slot:0, query, value } = item else { return Err(VerificationError::Invalid); };
        if query.as_str() != "search" { return Err(VerificationError::Invalid); }
        let value: Value = serde_json::from_str(value.as_str()).map_err(|_| VerificationError::Invalid)?;
        let text = value.as_str().ok_or(VerificationError::Invalid)?;
        lines.push(format!("{}. {}", index + 1, text));
    }
    Ok(if lines.is_empty() { "No results found.".into() } else { lines.join("\n\n") })
}

impl LiveConnectorSelectionPolicy for SearchConnectorProvider {
    fn supports(&self, tool: &str) -> bool { tool == "web_search" }
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        if context.record().employee != self.employee || tool != "web_search" || provider != "search" { return Err(VerificationError::BindingMismatch); }
        Ok(vec![self.account.clone()])
    }
}
impl BoundConnectorExecutor for SearchConnectorProvider {
    fn supports(&self, tool: &str) -> bool { tool == "web_search" }
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(self.run(selected, call))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_never_controls_origin_and_dropping_invocation_cancels_derived_work() {
        for query in ["hello world", "https://private.example/path", "&q=other#fragment", "繁體中文"] {
            let url = search_url(query).unwrap();
            assert_eq!(url.origin().ascii_serialization(), "https://html.duckduckgo.com");
            assert_eq!(url.query_pairs().collect::<Vec<_>>(), vec![("q".into(), query.into())]);
        }
        let invocation = Invocation::new();
        let child = invocation.active.clone();
        assert!(child.load(Ordering::SeqCst));
        drop(invocation);
        assert!(!child.load(Ordering::SeqCst));
    }
}
