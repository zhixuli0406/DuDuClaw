//! Decode the service extension separately from its MCP transport envelope.
use super::*;

pub(super) fn decode_response<T: serde::de::DeserializeOwned>(
    mut response: Value,
    modern: bool,
) -> Result<T, McpError> {
    // Bound the complete wire frame before discarding transport metadata.
    duduclaw_core::workflow_mcp::canonical_bytes(&response).map_err(McpError::Parse)?;
    if let Some(mut error) = rpc_error_of(&response) {
        // Only the authenticated local workflow extension restores its known
        // denial code. Generic/remote MCP errors retain their wire code.
        if modern
            && response.pointer("/error/code").and_then(Value::as_i64) == Some(-32000)
            && response.pointer("/error/data/duduclawCode").and_then(Value::as_i64) == Some(-32003)
            && let McpError::Rpc { code, .. } = &mut error
        {
            *code = -32003;
        }
        return Err(error);
    }
    let result = response.get_mut("result")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| McpError::Parse("missing workflow result".into()))?;
    if modern {
        // A workflow effect is single-attempt and never enters MRTR. Strip only
        // the protocol envelope, keeping all DTO fields subject to strict serde.
        if result.get("resultType").and_then(Value::as_str) != Some("complete") {
            return Err(McpError::Parse("workflow result must be complete".into()));
        }
        if result.get("_meta").is_some_and(|meta| !meta.is_object()) {
            return Err(McpError::Parse("invalid workflow response metadata".into()));
        }
        result.remove("resultType");
        result.remove("_meta");
    }
    let version = result.get("version")
        .or_else(|| result.get("effective").and_then(|effective| effective.get("version")))
        .and_then(Value::as_u64);
    if version != Some(u64::from(duduclaw_core::workflow_mcp::VERSION)) {
        return Err(McpError::Parse("unknown workflow response version".into()));
    }
    serde_json::from_value(Value::Object(std::mem::take(result)))
        .map_err(|_| McpError::Parse("invalid typed workflow response".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::workflow_mcp::{self, OperationReply, PrepareTicket, ReadReply};

    fn modern(result: Value) -> Value {
        let mut response = json!({"jsonrpc":"2.0","id":1,"result":result});
        response["result"]["resultType"] = json!("complete");
        response["result"]["_meta"] = json!({
            "io.modelcontextprotocol/serverInfo":{"name":"duduclaw","version":"fixture"}
        });
        response
    }

    fn prepare() -> Value {
        json!({"effective":{
            "version":1,"actor":"alice","session_id":"session","run_id":"run","step_key":"step",
            "tool":"tasks_update","effective_arguments":{"task_id":"target","title":"after"},
            "approval_metadata":{
                "action_grant":{"agent_id":"alice","tool":"tasks_update"},
                "egress_guard":{"reason":"egress_guard","sources":["memory_read"]}
            },
            "payload_hash":"payload","policy_revision":"policy","authority_digest":"authority",
            "environment_hash":"environment","approval_requirements":["human"],"expires_at":"deadline"
        },"nonce":"nonce","mac":"mac"})
    }

    #[test]
    fn modern_workflow_envelope_preserves_typed_payload_and_signature_bytes() {
        let original = prepare();
        let expected: PrepareTicket = serde_json::from_value(original.clone()).unwrap();
        let actual: PrepareTicket = decode_response(modern(original), true).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(workflow_mcp::digest(&actual).unwrap(), workflow_mcp::digest(&expected).unwrap());
        let key = [17; 32];
        let signature = workflow_mcp::sign(&key, "prepare", &workflow_mcp::unsigned(&expected).unwrap()).unwrap();
        workflow_mcp::verify(&key, "prepare", &workflow_mcp::unsigned(&actual).unwrap(), &signature).unwrap();

        let read = json!({
            "version":1,"run_id":"run","step_key":"step","tool":"web_fetch_cached","actor":"alice",
            "payload_hash":"payload","policy_revision":"policy","authority_digest":"authority",
            "environment_hash":"environment","observed_at":"now","result":{"body":"data"},
            "result_hash":"hash","evidence":{"source":"fixture"},"error_code":null
        });
        let actual: ReadReply = decode_response(modern(read.clone()), true).unwrap();
        assert_eq!(serde_json::to_value(actual).unwrap(), read);
        let operation = json!({
            "version":1,"operation_id":"operation","binding_digest":"binding",
            "authority_source":{"kind":"bound_human_approval","approval_id":"approval"},
            "state":"succeeded","receipt_digest":"receipt","result":{"title":"after"},"error_code":null
        });
        let actual: OperationReply = decode_response(modern(operation.clone()), true).unwrap();
        assert_eq!(serde_json::to_value(actual).unwrap(), operation);
    }

    #[test]
    fn workflow_denial_code_is_restored_only_for_the_modern_extension() {
        let denial = json!({"jsonrpc":"2.0","id":1,"error":{
            "code":-32000,"message":"workflow public URL rejected","data":{"duduclawCode":-32003}
        }});
        assert!(matches!(decode_response::<ReadReply>(denial.clone(), true),
            Err(McpError::Rpc { code: -32003, message }) if message == "workflow public URL rejected"));
        assert!(matches!(decode_response::<ReadReply>(denial.clone(), false),
            Err(McpError::Rpc { code: -32000, .. })));
        // The generic decoder must never reinterpret a third-party error.
        assert!(matches!(rpc_error_of(&denial), Some(McpError::Rpc { code: -32000, .. })));
        for code in [json!(-32022), json!(0), json!("-32003"), Value::Null] {
            let mut unknown = denial.clone();
            unknown["error"]["data"]["duduclawCode"] = code;
            assert!(matches!(decode_response::<ReadReply>(unknown, true),
                Err(McpError::Rpc { code: -32000, .. })));
        }
        let mut protocol_error = denial;
        protocol_error["error"]["code"] = json!(-32022);
        assert!(matches!(decode_response::<ReadReply>(protocol_error, true),
            Err(McpError::Rpc { code: -32022, .. })));
    }

    #[test]
    fn workflow_unknown_fields_and_incomplete_results_remain_refused() {
        let valid = modern(prepare());
        for result_type in [json!("input_required"), json!("future"), Value::Null, json!(true)] {
            let mut invalid = valid.clone();
            invalid["result"]["resultType"] = result_type;
            assert!(decode_response::<PrepareTicket>(invalid, true).is_err());
        }
        for metadata in [Value::Null, json!([]), json!("metadata")] {
            let mut invalid = valid.clone();
            invalid["result"]["_meta"] = metadata;
            assert!(decode_response::<PrepareTicket>(invalid, true).is_err());
        }
        for pointer in ["/result", "/result/effective", "/result/effective/approval_metadata"] {
            let mut forged = valid.clone();
            forged.pointer_mut(pointer).unwrap()["approved"] = json!(true);
            assert!(decode_response::<PrepareTicket>(forged, true).is_err());
        }
        let mut wrong_version = valid.clone();
        wrong_version["result"]["effective"]["version"] = json!(2);
        assert!(decode_response::<PrepareTicket>(wrong_version, true).is_err());
        let mut oversized = valid;
        oversized["result"]["_meta"]["padding"] = json!("x".repeat(workflow_mcp::MAX_FRAME_BYTES));
        assert!(decode_response::<PrepareTicket>(oversized, true).is_err());
    }

    #[test]
    fn legacy_workflow_payloads_and_denials_are_unchanged() {
        let result = prepare();
        let actual: PrepareTicket = decode_response(json!({"jsonrpc":"2.0","id":1,"result":result}), false).unwrap();
        assert_eq!(serde_json::to_value(actual).unwrap(), result);
        assert!(decode_response::<PrepareTicket>(modern(prepare()), false).is_err());
        assert!(matches!(decode_response::<ReadReply>(json!({"jsonrpc":"2.0","id":1,
            "error":{"code":-32003,"message":"refused"}}), false),
            Err(McpError::Rpc { code: -32003, message }) if message == "refused"));
    }
}
