//! gRPC server reflection, the two queries websec needs.
//!
//! Only a slice of the reflection proto is modelled, as flat optional fields at
//! the real tags. A oneof is wire-identical to its fields set one at a time, so
//! these encode and decode exactly like the generated types without the deps.

use anyhow::{Result, bail};
use prost::Message;

#[derive(Clone, PartialEq, Message)]
struct ServerReflectionRequest {
    #[prost(string, tag = "1")]
    host: String,
    #[prost(string, optional, tag = "4")]
    file_containing_symbol: Option<String>,
    #[prost(string, optional, tag = "7")]
    list_services: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct ServerReflectionResponse {
    #[prost(message, optional, tag = "4")]
    file_descriptor_response: Option<FileDescriptorResponse>,
    #[prost(message, optional, tag = "6")]
    list_services_response: Option<ListServiceResponse>,
    #[prost(message, optional, tag = "7")]
    error_response: Option<ErrorResponse>,
}

#[derive(Clone, PartialEq, Message)]
struct FileDescriptorResponse {
    #[prost(bytes = "vec", repeated, tag = "1")]
    file_descriptor_proto: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
struct ListServiceResponse {
    #[prost(message, repeated, tag = "1")]
    service: Vec<ServiceResponse>,
}

#[derive(Clone, PartialEq, Message)]
struct ServiceResponse {
    #[prost(string, tag = "1")]
    name: String,
}

#[derive(Clone, PartialEq, Message)]
struct ErrorResponse {
    #[prost(int32, tag = "1")]
    error_code: i32,
    #[prost(string, tag = "2")]
    error_message: String,
}

/// What a reflection exchange yielded, folded across its response messages.
#[derive(Debug, Default, Clone)]
pub struct ReflectionReply {
    /// `FileDescriptorProto` bytes, for [`crate::Descriptors::from_file_descriptor_protos`].
    pub file_descriptor_protos: Vec<Vec<u8>>,
    /// Service names, when the query was `list_services`.
    pub services: Vec<String>,
    /// The server's own error, if it sent one (e.g. symbol not found).
    pub error: Option<String>,
}

/// Encode a request asking for the file that defines `symbol`.
pub fn symbol_request(symbol: &str) -> Vec<u8> {
    ServerReflectionRequest {
        file_containing_symbol: Some(symbol.to_string()),
        ..Default::default()
    }
    .encode_to_vec()
}

/// Encode a request asking for the list of services.
pub fn list_services_request() -> Vec<u8> {
    ServerReflectionRequest {
        list_services: Some(String::new()),
        ..Default::default()
    }
    .encode_to_vec()
}

/// Fold the reply's response messages into one [`ReflectionReply`].
pub fn parse_reflection_reply(messages: &[Vec<u8>]) -> Result<ReflectionReply> {
    let mut reply = ReflectionReply::default();
    for raw in messages {
        let resp = ServerReflectionResponse::decode(raw.as_slice())
            .map_err(|e| anyhow::anyhow!("a reflection response did not decode: {e}"))?;
        if let Some(fdr) = resp.file_descriptor_response {
            reply.file_descriptor_protos.extend(fdr.file_descriptor_proto);
        }
        if let Some(lsr) = resp.list_services_response {
            reply.services.extend(lsr.service.into_iter().map(|s| s.name));
        }
        if let Some(err) = resp.error_response {
            reply.error = Some(format!("{} (code {})", err.error_message, err.error_code));
        }
    }
    if reply.file_descriptor_protos.is_empty()
        && reply.services.is_empty()
        && reply.error.is_none()
    {
        bail!("the reflection reply carried no descriptors, services or error");
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symbol_request_round_trips_through_the_wire_shape() {
        let bytes = symbol_request("pkg.Svc");
        let back = ServerReflectionRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(back.file_containing_symbol.as_deref(), Some("pkg.Svc"));
    }

    #[test]
    fn a_list_reply_is_folded_to_service_names() {
        let resp = ServerReflectionResponse {
            list_services_response: Some(ListServiceResponse {
                service: vec![ServiceResponse { name: "pkg.Svc".into() }],
            }),
            ..Default::default()
        };
        let reply = parse_reflection_reply(&[resp.encode_to_vec()]).unwrap();
        assert_eq!(reply.services, vec!["pkg.Svc".to_string()]);
    }
}
