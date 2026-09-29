//! A `.proto` in, a described service and a JSON<->protobuf round trip out.

use h5i_grpc::Descriptors;

const PROTO: &str = r#"
syntax = "proto3";
package demo;

message EchoRequest { string text = 1; int32 count = 2; }
message EchoReply { string text = 1; }

service Echo {
  rpc Say(EchoRequest) returns (EchoReply);
  rpc Stream(EchoRequest) returns (stream EchoReply);
}
"#;

fn descriptors() -> Descriptors {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("demo.proto");
    std::fs::write(&path, PROTO).expect("write proto");
    Descriptors::from_proto(&[path], &[] as &[&std::path::Path]).expect("compiles")
}

#[test]
fn a_proto_describes_its_service_and_streaming_flags() {
    let d = descriptors();
    let described = d.services();
    let svc = described
        .services
        .iter()
        .find(|s| s.name == "demo.Echo")
        .expect("Echo is listed");
    assert_eq!(svc.methods.len(), 2);
    let stream = svc.methods.iter().find(|m| m.name == "Stream").unwrap();
    assert!(stream.server_streaming);
    assert!(!stream.client_streaming);
    assert_eq!(stream.full, "demo.Echo/Stream");
}

#[test]
fn json_encodes_to_protobuf_and_a_response_decodes_back() {
    let d = descriptors();
    let method = d.method("demo.Echo/Say").expect("method resolves");
    assert_eq!(method.path, "/demo.Echo/Say");

    let wire = method.encode_input(r#"{"text":"hi","count":3}"#).expect("encodes");
    assert!(!wire.is_empty());

    // Feed the same bytes back as if they were the reply (Reply shares `text`).
    let json = method.decode_output(&wire).expect("decodes");
    assert_eq!(json["text"], "hi");
}

#[test]
fn a_bad_request_field_is_refused_not_dropped() {
    let d = descriptors();
    let method = d.method("demo.Echo/Say").unwrap();
    assert!(method.encode_input(r#"{"nope":"x"}"#).is_err());
}

#[test]
fn a_method_path_helper_takes_slash_or_dot() {
    assert_eq!(h5i_grpc::method_path("demo.Echo/Say").unwrap(), "/demo.Echo/Say");
    assert_eq!(h5i_grpc::method_path("demo.Echo.Say").unwrap(), "/demo.Echo/Say");
}
