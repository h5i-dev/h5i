//! The descriptor pool: load it, describe it, and find a method to call.

use std::path::Path;

use anyhow::{Context, Result};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use prost_types::{FileDescriptorProto, FileDescriptorSet};
use serde::Serialize;

use crate::split_method;

/// A loaded set of descriptors, however they arrived.
#[derive(Clone)]
pub struct Descriptors {
    pool: DescriptorPool,
}

/// One service and the methods it exposes, for `describe`.
#[derive(Serialize)]
pub struct ServiceInfo {
    pub name: String,
    pub methods: Vec<MethodSummary>,
}

/// One method, named the way both `describe` and a call want it.
#[derive(Serialize)]
pub struct MethodSummary {
    pub name: String,
    /// `pkg.Service/Method`, ready to paste into `grpc call`.
    pub full: String,
    pub input: String,
    pub output: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
}

/// Every service in the pool.
#[derive(Serialize)]
pub struct Describe {
    pub services: Vec<ServiceInfo>,
}

/// A method resolved for a call: its path and the descriptors to (de)code with.
pub struct MethodInfo {
    /// `/pkg.Service/Method`, the HTTP/2 `:path`.
    pub path: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
    input: MessageDescriptor,
    output: MessageDescriptor,
}

impl MethodInfo {
    /// A JSON request as the wire bytes of the input message (not yet framed).
    pub fn encode_input(&self, json: &str) -> Result<Vec<u8>> {
        let mut de = serde_json::Deserializer::from_str(json);
        let msg = DynamicMessage::deserialize(self.input.clone(), &mut de)
            .context("the request JSON does not fit the method's input message")?;
        de.end().context("trailing data after the request JSON")?;
        Ok(msg.encode_to_vec())
    }

    /// One response message's wire bytes as JSON.
    pub fn decode_output(&self, bytes: &[u8]) -> Result<serde_json::Value> {
        let msg = DynamicMessage::decode(self.output.clone(), bytes)
            .context("the response bytes do not fit the method's output message")?;
        Ok(serde_json::to_value(&msg)?)
    }
}

impl Descriptors {
    /// Compile one or more `.proto` files into a pool. `includes` are the import
    /// roots; the files' own directories are added so a bare path just works.
    pub fn from_proto(files: &[impl AsRef<Path>], includes: &[impl AsRef<Path>]) -> Result<Self> {
        let mut roots: Vec<std::path::PathBuf> =
            includes.iter().map(|p| p.as_ref().to_path_buf()).collect();
        for f in files {
            if let Some(dir) = f.as_ref().parent() {
                roots.push(dir.to_path_buf());
            }
        }
        let files: Vec<std::path::PathBuf> =
            files.iter().map(|p| p.as_ref().to_path_buf()).collect();
        let fds = protox::compile(&files, &roots).context("compiling the .proto failed")?;
        // Route through bytes so this pool's prost-types is the one that decodes,
        // whatever protox was built against.
        Self::from_descriptor_set_bytes(&fds.encode_to_vec())
    }

    /// Load a protoset: a serialized `FileDescriptorSet` (e.g. `protoc -o`).
    pub fn from_descriptor_set_bytes(bytes: &[u8]) -> Result<Self> {
        let pool = DescriptorPool::decode(bytes)
            .context("this is not a FileDescriptorSet (a protoset from `protoc -o` or reflection)")?;
        Ok(Self { pool })
    }

    /// Build a pool from the `FileDescriptorProto` bytes a reflection query
    /// returned, de-duplicated by file name and resolved as one set.
    pub fn from_file_descriptor_protos(protos: &[Vec<u8>]) -> Result<Self> {
        let mut files: Vec<FileDescriptorProto> = Vec::new();
        for raw in protos {
            let file = FileDescriptorProto::decode(raw.as_slice())
                .context("a reflected file descriptor did not decode")?;
            if !files.iter().any(|f| f.name == file.name) {
                files.push(file);
            }
        }
        let set = FileDescriptorSet { file: files };
        Self::from_descriptor_set_bytes(&set.encode_to_vec())
    }

    /// Resolve a method for a call.
    pub fn method(&self, full: &str) -> Result<MethodInfo> {
        let (service, method) = split_method(full)?;
        let svc = self
            .pool
            .get_service_by_name(&service)
            .with_context(|| format!("no service `{service}` in these descriptors"))?;
        let m = svc
            .methods()
            .find(|m| m.name() == method)
            .with_context(|| format!("service `{service}` has no method `{method}`"))?;
        Ok(MethodInfo {
            path: format!("/{service}/{method}"),
            client_streaming: m.is_client_streaming(),
            server_streaming: m.is_server_streaming(),
            input: m.input(),
            output: m.output(),
        })
    }

    /// Every service and its methods.
    pub fn services(&self) -> Describe {
        let services = self
            .pool
            .services()
            .map(|svc| ServiceInfo {
                name: svc.full_name().to_string(),
                methods: svc
                    .methods()
                    .map(|m| MethodSummary {
                        name: m.name().to_string(),
                        full: format!("{}/{}", svc.full_name(), m.name()),
                        input: m.input().full_name().to_string(),
                        output: m.output().full_name().to_string(),
                        client_streaming: m.is_client_streaming(),
                        server_streaming: m.is_server_streaming(),
                    })
                    .collect(),
            })
            .collect();
        Describe { services }
    }

    /// Describe one symbol: a service, a message, or a method's messages.
    pub fn describe_symbol(&self, symbol: &str) -> Result<serde_json::Value> {
        let symbol = symbol.trim().trim_start_matches('/');
        if let Some(svc) = self.pool.get_service_by_name(symbol) {
            let info = ServiceInfo {
                name: svc.full_name().to_string(),
                methods: svc
                    .methods()
                    .map(|m| MethodSummary {
                        name: m.name().to_string(),
                        full: format!("{}/{}", svc.full_name(), m.name()),
                        input: m.input().full_name().to_string(),
                        output: m.output().full_name().to_string(),
                        client_streaming: m.is_client_streaming(),
                        server_streaming: m.is_server_streaming(),
                    })
                    .collect(),
            };
            return Ok(serde_json::json!({ "service": info }));
        }
        if let Some(msg) = self.pool.get_message_by_name(symbol) {
            return Ok(serde_json::json!({ "message": message_schema(&msg) }));
        }
        // A `pkg.Service/Method` symbol: describe its two messages.
        if let Ok((service, method)) = split_method(symbol)
            && let Some(svc) = self.pool.get_service_by_name(&service)
            && let Some(m) = svc.methods().find(|m| m.name() == method)
        {
            return Ok(serde_json::json!({
                "method": m.full_name(),
                "input": message_schema(&m.input()),
                "output": message_schema(&m.output()),
            }));
        }
        anyhow::bail!("`{symbol}` is not a service, message or method in these descriptors")
    }
}

/// A message's fields, enough to hand-write a request JSON.
fn message_schema(msg: &MessageDescriptor) -> serde_json::Value {
    let fields: Vec<serde_json::Value> = msg
        .fields()
        .map(|f| {
            serde_json::json!({
                "name": f.name(),
                "number": f.number(),
                "type": format!("{:?}", f.kind()),
                "repeated": f.is_list(),
            })
        })
        .collect();
    serde_json::json!({ "name": msg.full_name(), "fields": fields })
}
