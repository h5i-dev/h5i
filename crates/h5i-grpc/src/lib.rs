//! gRPC for h5i websec, without a socket.
//!
//! [`Descriptors`] holds a `prost_reflect` pool built from a `.proto`, a
//! protoset (`FileDescriptorSet`), or the bytes a reflection query returned. It
//! turns a JSON request into protobuf and a protobuf response back into JSON,
//! and describes what a service exposes. Framing wraps a message the way gRPC
//! frames it. The transport that carries those bytes is `h5i browser grpc`.

mod codec;
mod descriptor;
mod reflection;

pub use codec::{frame, unframe};
pub use descriptor::{Descriptors, MethodInfo, Describe, ServiceInfo, MethodSummary};
pub use reflection::{
    ReflectionReply, list_services_request, symbol_request, parse_reflection_reply,
};

/// The gRPC path for a method, as `full` names it: `pkg.Service/Method`.
///
/// Accepts either separator between service and method (`/` or a final `.`), so
/// a symbol copied from `describe` and a path copied from a proxy both work.
pub fn method_path(full: &str) -> anyhow::Result<String> {
    let (service, method) = split_method(full)?;
    Ok(format!("/{service}/{method}"))
}

/// Split `pkg.Service/Method` (or `pkg.Service.Method`) into service and method.
pub(crate) fn split_method(full: &str) -> anyhow::Result<(String, String)> {
    let full = full.trim().trim_start_matches('/');
    let (service, method) = if let Some(at) = full.rfind('/') {
        (&full[..at], &full[at + 1..])
    } else if let Some(at) = full.rfind('.') {
        (&full[..at], &full[at + 1..])
    } else {
        anyhow::bail!("`{full}` is not `package.Service/Method`");
    };
    if service.is_empty() || method.is_empty() {
        anyhow::bail!("`{full}` is not `package.Service/Method`");
    }
    Ok((service.to_string(), method.to_string()))
}
