//! Integration tests for the JSON-RPC 2.0 wire envelopes exported by
//! `ainb-hangar-proto`. These prove the envelopes round-trip through JSON
//! byte-identically in shape and that the error envelope matches the
//! JSON-RPC 2.0 `{code, message, data?}` contract.

use ainb_hangar_proto::{RpcError, RpcId, RpcRequest, RpcResponse};

#[test]
fn rpc_envelope_roundtrips_through_json() {
    let req = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(7),
        method: "hangar/ping".to_string(),
        params: serde_json::Value::Null,
    };
    let encoded = serde_json::to_string(&req).expect("encode");
    let decoded: RpcRequest = serde_json::from_str(&encoded).expect("decode");
    assert_eq!(decoded, req);
}

#[test]
fn rpc_request_emits_jsonrpc_2_0_member() {
    let req = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(7),
        method: "hangar/ping".to_string(),
        params: serde_json::Value::Null,
    };
    let value = serde_json::to_value(&req).expect("encode");
    assert_eq!(value["jsonrpc"], serde_json::json!("2.0"));
    assert_eq!(value["id"], serde_json::json!(7));
    assert_eq!(value["method"], serde_json::json!("hangar/ping"));
}

#[test]
fn rpc_request_defaults_jsonrpc_when_absent_in_input() {
    // A peer that omits the member (lenient decode) still yields a 2.0 envelope.
    let decoded: RpcRequest =
        serde_json::from_str(r#"{"id":1,"method":"hangar/ping","params":null}"#)
            .expect("decode");
    assert_eq!(decoded.jsonrpc, "2.0");
}

#[test]
fn rpc_response_emits_jsonrpc_2_0_member() {
    let resp = RpcResponse {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(7),
        result: Some(serde_json::json!({"ok": true})),
        error: None,
    };
    let value = serde_json::to_value(&resp).expect("encode");
    assert_eq!(value["jsonrpc"], serde_json::json!("2.0"));
    assert_eq!(value["id"], serde_json::json!(7));
    assert_eq!(value["result"], serde_json::json!({"ok": true}));
    assert!(value.get("error").is_none());
}

#[test]
fn rpc_response_roundtrips_through_json() {
    let resp = RpcResponse {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Text("abc".to_string()),
        result: None,
        error: Some(RpcError {
            code: -32601,
            message: "method not found".to_string(),
            data: None,
        }),
    };
    let encoded = serde_json::to_string(&resp).expect("encode");
    let decoded: RpcResponse = serde_json::from_str(&encoded).expect("decode");
    assert_eq!(decoded, resp);
}

#[test]
fn error_envelope_carries_code_and_message() {
    let err = RpcError {
        code: -32601,
        message: "method not found".to_string(),
        data: None,
    };
    let value = serde_json::to_value(&err).expect("encode");
    assert_eq!(value["code"], serde_json::json!(-32601));
    assert_eq!(value["message"], serde_json::json!("method not found"));

    // With a data payload present, it carries through.
    let err2 = RpcError {
        code: -32000,
        message: "boom".to_string(),
        data: Some(serde_json::json!({"detail": "x"})),
    };
    let decoded: RpcError =
        serde_json::from_value(serde_json::to_value(&err2).expect("encode"))
            .expect("decode");
    assert_eq!(decoded, err2);
}
