//! `h5i websec grpc`: the front door for gRPC.
//!
//! Descriptors, describe and JSON<->protobuf are `h5i_grpc`'s, here in the
//! plugin. The socket is the engine's: every call goes out through
//! `h5i browser grpc`, so it is policed and recorded like a `resend`, and a
//! finding can cite it by the `req_<n>`/`res_<n>` id the reply carries.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use h5i_grpc::Descriptors;
use serde_json::Value;

use crate::{DescriptorSource, GrpcVerb};

/// The two reflection service paths, newest first.
const REFLECTION_PATHS: [&str; 2] = [
    "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
    "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
];

pub fn run(session: Option<&str>, command: &GrpcVerb, json_out: bool) -> Result<()> {
    match command {
        GrpcVerb::Describe { symbol, src } => describe(session, symbol.as_deref(), src, json_out),
        GrpcVerb::Call {
            method,
            data,
            server_streaming,
            metadata,
            src,
        } => call(session, method, data, *server_streaming, metadata, src, json_out),
    }
}

fn describe(
    session: Option<&str>,
    symbol: Option<&str>,
    src: &DescriptorSource,
    json_out: bool,
) -> Result<()> {
    // Reflection with no symbol is a list of services, which is all the server
    // will give without one to look up.
    if src.reflect && symbol.is_none() {
        let url = src.url.as_deref().context("--reflect needs --url")?;
        let services = reflect_list_services(session, url, src.insecure)?;
        if json_out {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "services": services }))?);
        } else if services.is_empty() {
            println!("the server listed no services");
        } else {
            println!("services:");
            for name in services {
                println!("  {name}");
            }
        }
        return Ok(());
    }

    let descriptors = resolve(session, src, symbol)?;
    let value = match symbol {
        Some(symbol) => descriptors.describe_symbol(symbol)?,
        None => serde_json::to_value(descriptors.services())?,
    };
    if json_out {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print_describe(&value);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn call(
    session: Option<&str>,
    method: &str,
    data: &str,
    server_streaming: bool,
    metadata: &[String],
    src: &DescriptorSource,
    json_out: bool,
) -> Result<()> {
    let url = src.url.as_deref().context("`grpc call` needs --url")?;
    // Reflection fetches the file that defines the method's own symbol.
    let descriptors = resolve(session, src, Some(method))?;
    let info = descriptors.method(method)?;
    let request = info.encode_input(data)?;
    let framed = h5i_grpc::frame(&request);

    let reply = call_engine(session, url, &info.path, &framed, metadata, src.insecure)?;

    let messages = h5i_grpc::unframe(&reply.frames)?;
    let mut decoded = Vec::with_capacity(messages.len());
    for message in &messages {
        decoded.push(info.decode_output(message)?);
    }

    let ok = reply.grpc_status == Some(0) || (reply.grpc_status.is_none() && !decoded.is_empty());
    if json_out {
        let out = serde_json::json!({
            "ok": ok,
            "grpc_status": reply.grpc_status,
            "grpc_message": reply.grpc_message,
            "http_status": reply.http_status,
            "messages": decoded,
            "request_id": reply.request_id,
            "response_id": reply.response_id,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    // Text: the status line, then each message, then the evidence id.
    match reply.grpc_status {
        Some(0) | None => {}
        Some(code) => {
            let message = reply.grpc_message.as_deref().unwrap_or("");
            println!("gRPC error {code} {}: {message}", grpc_status_name(code));
        }
    }
    if !server_streaming && decoded.len() > 1 {
        println!("(the server sent {} messages for a unary method)", decoded.len());
    }
    for message in &decoded {
        println!("{}", serde_json::to_string_pretty(message)?);
    }
    if decoded.is_empty() && reply.grpc_status.unwrap_or(0) == 0 {
        println!("(no response message)");
    }
    if let Some(id) = &reply.response_id {
        println!("evidence: {id}");
    }
    Ok(())
}

/// The engine's reply to one `h5i browser grpc` call.
struct EngineReply {
    grpc_status: Option<i64>,
    grpc_message: Option<String>,
    http_status: Option<u64>,
    frames: Vec<u8>,
    request_id: Option<String>,
    response_id: Option<String>,
}

/// Send one framed call through the engine and parse its reply.
fn call_engine(
    session: Option<&str>,
    url: &str,
    path: &str,
    framed: &[u8],
    metadata: &[String],
    insecure: bool,
) -> Result<EngineReply> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(framed);
    let mut args: Vec<String> = vec![
        "grpc".into(),
        url.into(),
        "--path".into(),
        path.into(),
        "--request-frame".into(),
        b64,
    ];
    for pair in metadata {
        args.push("--metadata".into());
        args.push(pair.clone());
    }
    if insecure {
        args.push("--insecure".into());
    }
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let reply = crate::ask_browser(&borrowed, session)?;

    if reply.get("ok").and_then(Value::as_bool) == Some(false) {
        let message = reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the engine refused the call");
        bail!("{message}");
    }
    let frames = match reply.get("response_frame_b64").and_then(Value::as_str) {
        Some(b64) => base64::engine::general_purpose::STANDARD
            .decode(b64)
            .context("the engine's response frames were not base64")?,
        None => Vec::new(),
    };
    Ok(EngineReply {
        grpc_status: reply.get("grpc_status").and_then(Value::as_i64),
        grpc_message: reply
            .get("grpc_message")
            .and_then(Value::as_str)
            .map(str::to_string),
        http_status: reply.get("http_status").and_then(Value::as_u64),
        frames,
        request_id: reply.get("request_id").and_then(Value::as_str).map(str::to_string),
        response_id: reply.get("response_id").and_then(Value::as_str).map(str::to_string),
    })
}

/// Resolve descriptors from a `.proto`, a protoset, or reflection.
fn resolve(
    session: Option<&str>,
    src: &DescriptorSource,
    symbol: Option<&str>,
) -> Result<Descriptors> {
    if !src.proto.is_empty() {
        return Descriptors::from_proto(&src.proto, &src.import_path);
    }
    if let Some(protoset) = &src.protoset {
        let bytes = std::fs::read(protoset)
            .with_context(|| format!("could not read the protoset `{protoset}`"))?;
        return Descriptors::from_descriptor_set_bytes(&bytes);
    }
    if src.reflect {
        let url = src.url.as_deref().context("--reflect needs --url")?;
        let symbol = symbol.context("reflection needs a symbol (a service or method) to fetch")?;
        return reflect_symbol(session, url, symbol, src.insecure);
    }
    bail!("no descriptors: pass --proto, --protoset, or --reflect")
}

/// Ask the server, over reflection, for the file that defines `symbol`.
fn reflect_symbol(
    session: Option<&str>,
    url: &str,
    symbol: &str,
    insecure: bool,
) -> Result<Descriptors> {
    // The server looks up a real protobuf symbol, so a `Service/Method` path
    // becomes the method's dotted name `Service.Method`.
    let symbol = symbol.replacen('/', ".", 1);
    let symbol = symbol.as_str();
    let request = h5i_grpc::frame(&h5i_grpc::symbol_request(symbol));
    let reply = reflect_call(session, url, &request, insecure)?;
    if let Some(error) = reply.error {
        bail!("the server's reflection API refused `{symbol}`: {error}");
    }
    if reply.file_descriptor_protos.is_empty() {
        bail!("reflection returned no descriptors for `{symbol}`");
    }
    Descriptors::from_file_descriptor_protos(&reply.file_descriptor_protos)
}

/// Ask the server for its list of services.
fn reflect_list_services(session: Option<&str>, url: &str, insecure: bool) -> Result<Vec<String>> {
    let request = h5i_grpc::frame(&h5i_grpc::list_services_request());
    let reply = reflect_call(session, url, &request, insecure)?;
    if let Some(error) = reply.error {
        bail!("the server's reflection API refused the request: {error}");
    }
    Ok(reply.services)
}

/// One reflection exchange, trying v1 then v1alpha.
fn reflect_call(
    session: Option<&str>,
    url: &str,
    framed_request: &[u8],
    insecure: bool,
) -> Result<h5i_grpc::ReflectionReply> {
    let mut last = String::new();
    for path in REFLECTION_PATHS {
        let reply = call_engine(session, url, path, framed_request, &[], insecure)?;
        // Unimplemented (12) means this reflection version is not served; try
        // the next path rather than reporting it as the answer.
        if reply.grpc_status == Some(12) {
            last = reply
                .grpc_message
                .clone()
                .unwrap_or_else(|| "unimplemented".to_string());
            continue;
        }
        let messages = h5i_grpc::unframe(&reply.frames)?;
        return h5i_grpc::parse_reflection_reply(&messages);
    }
    bail!("the server does not serve gRPC reflection ({last}); pass --proto or --protoset instead")
}

/// A short name for a gRPC status code, for the error line.
fn grpc_status_name(code: i64) -> &'static str {
    match code {
        0 => "OK",
        1 => "CANCELLED",
        2 => "UNKNOWN",
        3 => "INVALID_ARGUMENT",
        4 => "DEADLINE_EXCEEDED",
        5 => "NOT_FOUND",
        6 => "ALREADY_EXISTS",
        7 => "PERMISSION_DENIED",
        8 => "RESOURCE_EXHAUSTED",
        9 => "FAILED_PRECONDITION",
        10 => "ABORTED",
        11 => "OUT_OF_RANGE",
        12 => "UNIMPLEMENTED",
        13 => "INTERNAL",
        14 => "UNAVAILABLE",
        15 => "DATA_LOSS",
        16 => "UNAUTHENTICATED",
        _ => "UNKNOWN",
    }
}

/// A plain-text rendering of a `describe` value.
fn print_describe(value: &Value) {
    if let Some(services) = value.get("services").and_then(Value::as_array) {
        for service in services {
            print_service(service);
        }
    } else if let Some(service) = value.get("service") {
        print_service(service);
    } else if let Some(message) = value.get("message") {
        print_message(message);
    } else if value.get("method").is_some() {
        if let Some(name) = value.get("method").and_then(Value::as_str) {
            println!("method {name}");
        }
        if let Some(input) = value.get("input") {
            print!("  input ");
            print_message(input);
        }
        if let Some(output) = value.get("output") {
            print!("  output ");
            print_message(output);
        }
    } else {
        println!("{value}");
    }
}

fn print_service(service: &Value) {
    let name = service.get("name").and_then(Value::as_str).unwrap_or("?");
    println!("service {name}");
    if let Some(methods) = service.get("methods").and_then(Value::as_array) {
        for method in methods {
            let mname = method.get("name").and_then(Value::as_str).unwrap_or("?");
            let input = method.get("input").and_then(Value::as_str).unwrap_or("?");
            let output = method.get("output").and_then(Value::as_str).unwrap_or("?");
            let cs = method.get("client_streaming").and_then(Value::as_bool).unwrap_or(false);
            let ss = method.get("server_streaming").and_then(Value::as_bool).unwrap_or(false);
            let inm = if cs { "stream " } else { "" };
            let outm = if ss { "stream " } else { "" };
            println!("  {mname}({inm}{input}) returns ({outm}{output})");
        }
    }
}

fn print_message(message: &Value) {
    let name = message.get("name").and_then(Value::as_str).unwrap_or("?");
    println!("message {name}");
    if let Some(fields) = message.get("fields").and_then(Value::as_array) {
        for field in fields {
            let fname = field.get("name").and_then(Value::as_str).unwrap_or("?");
            let number = field.get("number").and_then(Value::as_i64).unwrap_or(0);
            let ty = field.get("type").and_then(Value::as_str).unwrap_or("?");
            let repeated = field.get("repeated").and_then(Value::as_bool).unwrap_or(false);
            let rep = if repeated { "repeated " } else { "" };
            println!("  {number}: {rep}{ty} {fname}");
        }
    }
}
