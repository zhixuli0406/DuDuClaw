use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
pub(super) struct Probe {
    pub deny: AtomicBool,
    pub after: AtomicUsize,
    pub args: std::sync::Mutex<Value>,
    pub released: Arc<AtomicUsize>,
}

struct ProbeLease {
    released: Arc<AtomicUsize>,
    phase: AtomicUsize,
}

impl ClientCallLease for ProbeLease {
    fn dispatched(&self) { self.phase.store(1, Ordering::SeqCst); }
    fn terminal_response(&self) { self.phase.store(2, Ordering::SeqCst); }
}

impl Drop for ProbeLease {
    fn drop(&mut self) {
        if self.phase.load(Ordering::SeqCst) != 1 {
            self.released.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[async_trait]
impl ClientCallBoundary for Probe {
    async fn before_call(&self, _: &str, args: &Value) -> Result<(), String> {
        *self.args.lock().unwrap() = args.clone();
        if self.deny.load(Ordering::SeqCst) {
            Err("boundary refused".into())
        } else {
            Ok(())
        }
    }
    async fn begin_scoped_call(
        &self, tool: &str, args: &Value,
    ) -> Result<(Option<String>, Option<Box<dyn ClientCallLease>>), String> {
        self.before_call(tool, args).await?;
        Ok((None, Some(Box::new(ProbeLease { released: self.released.clone(), phase: AtomicUsize::new(0) }))))
    }
    async fn after_success(&self, _: &str) -> Result<(), String> {
        self.after.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn egress_error_shaped_responses_are_observed_before_delivery() {
    let probe = Probe::default();
    let is_error = parse_tool_call_result(&json!({"result":{"isError":true,"content":[{"type":"text","text":"private"}]}}));
    assert!(is_error.as_ref().unwrap().is_error);
    let rpc = Err(rpc_error_of(&json!({"error":{"code":-1,"message":"private error","data":{"secret":42}}})).unwrap());
    for result in [is_error, rpc, Err(McpError::Io("HTTP 500: private record".into())), Err(McpError::Parse("private diagnostic".into()))] {
        let payload = model_visible_tool_reply(&result);
        assert!(payload);
        probe.finish_call("read", None, payload).await.unwrap();
    }
    assert_eq!(probe.after.load(Ordering::SeqCst), 4);
    assert!(!model_visible_tool_reply(&Err(McpError::Closed)));
    assert!(!model_visible_tool_reply(&Err(McpError::Timeout)));
}

#[cfg(unix)]
#[tokio::test]
async fn egress_stdio_transport_terminal_releases_but_timeout_and_abort_retain() {
    let script = r#"
import json, sys, time
for line in sys.stdin:
    req = json.loads(line)
    if 'id' not in req: continue
    method = req.get('method')
    if method == 'initialize':
        result = {'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'test','version':'1'}}
    elif method == 'tools/list':
        result = {'tools':[{'name':'send','inputSchema':{'type':'object'}}]}
    else:
        case = req['params']['arguments']['case']
        with open(sys.argv[1], 'w') as accepted: accepted.write(case)
        if case in ['timeout','abort']: time.sleep(2)
        if case == 'error':
            print(json.dumps({'jsonrpc':'2.0','id':req['id'],'error':{'code':-1,'message':'terminal'}}), flush=True)
            continue
        result = {'content':[{'type':'text','text':'done'}]}
    print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':result}), flush=True)
"#;
    for case in ["success", "error", "timeout", "abort"] {
        let home = tempfile::tempdir().unwrap();
        let accepted = home.path().join("accepted");
        let mut client = McpClient::connect("/usr/bin/python3", &["-u".into(), "-c".into(), script.into(), accepted.to_string_lossy().into_owned()], &[], Duration::from_secs(5)).await.unwrap();
        client.timeout = if case == "abort" { Duration::from_secs(5) } else { Duration::from_millis(100) };
        let mut registry = ToolRegistry::from_clients_named(vec![("test".into(), client)], vec![]).await.unwrap();
        let boundary = Arc::new(Probe::default());
        registry.register_client_boundary(0, boundary.clone()).unwrap();
        if case == "abort" {
            let task = tokio::spawn(async move { registry.call("send", json!({"case":"abort"})).await });
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if tokio::fs::read_to_string(&accepted).await.ok().as_deref() == Some("abort") { break; }
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            let result = registry.call("send", json!({"case":case})).await;
            match case {
                "success" => assert!(!result.unwrap().is_error),
                "error" => assert_eq!(result.unwrap_err(), "mcp rpc error -1: terminal"),
                _ => assert_eq!(result.unwrap_err(), "mcp request timed out"),
            }
        }
        assert_eq!(std::fs::read_to_string(accepted).unwrap(), case, "upstream accepted the frame");
        assert_eq!(boundary.released.load(Ordering::SeqCst), usize::from(matches!(case, "success" | "error")), "{case}");
    }
}
