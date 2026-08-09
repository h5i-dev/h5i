//! The native primitives, and nothing above them.
//!
//! Every function here takes and returns node **ids** and strings. There is no
//! `Element` type on this side, because there is no second tree: `prelude.js`
//! builds the DOM object model on top of these, so a JS object that names a node
//! is a wrapper around a number and the Blitz document stays the only truth.
//!
//! Two rules hold throughout.
//!
//! **Borrow, act, release.** Each primitive takes its `RefCell` borrow, does its
//! work, and drops it before returning to JavaScript. Blitz mutations never call
//! back into script, so nothing can re-enter while a borrow is held.
//!
//! **Never invent a node.** A primitive handed an id that is gone returns
//! `null`, and the prelude turns that into the same thing a browser would. An id
//! that silently resolved to the wrong node would corrupt the snapshot, the
//! paint and the agent's model at once.

use boa_engine::{js_string, Context, JsArgs, JsError, JsResult, JsValue, NativeFunction};

use super::host::{ConsoleLine, HostHandle};

/// Read the host out of the context. Every primitive starts here.
fn host(context: &mut Context) -> JsResult<HostHandle> {
    context
        .get_data::<HostHandle>()
        .cloned()
        .ok_or_else(|| JsError::from_opaque(js_string!("the script realm has no document").into()))
}

fn arg_string(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<String> {
    Ok(args
        .get_or_undefined(index)
        .to_string(context)?
        .to_std_string_escaped())
}

fn arg_id(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<usize> {
    Ok(args.get_or_undefined(index).to_number(context)? as usize)
}

fn id_value(id: Option<usize>) -> JsValue {
    match id {
        Some(id) => JsValue::from(id as f64),
        None => JsValue::null(),
    }
}

/// Install every primitive under a single global the prelude reads from.
pub fn install(context: &mut Context) -> JsResult<()> {
    type Primitive = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;
    let primitives: &[(&str, usize, Primitive)] = &[
        ("query", 2, query),
        ("queryAll", 2, query_all),
        ("createElement", 1, create_element),
        ("createText", 1, create_text),
        ("append", 2, append),
        ("insertBefore", 2, insert_before),
        ("removeNode", 1, remove_node),
        ("setText", 2, set_text),
        ("getText", 1, get_text),
        ("setAttr", 3, set_attr),
        ("getAttr", 2, get_attr),
        ("removeAttr", 2, remove_attr),
        ("tagName", 1, tag_name),
        ("children", 1, children),
        ("parent", 1, parent),
        ("isElement", 1, is_element),
        ("root", 0, root),
        ("body", 0, body),
        ("setInnerHtml", 2, set_inner_html),
        ("getValue", 1, get_value),
        ("setValue", 2, set_value),
        ("log", 2, log),
        ("unsupported", 1, unsupported),
        ("fetch", 3, fetch),
        ("innerHtml", 1, inner_html),
        ("outerHtml", 1, outer_html),
        ("rect", 1, rect),
        ("computedStyle", 2, computed_style),
        ("parseUrl", 2, parse_url),
    ];

    let api = boa_engine::object::ObjectInitializer::new(context).build();
    for (name, arity, function) in primitives {
        let callable = boa_engine::object::FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_fn_ptr(*function),
        )
        .name(*name)
        .length(*arity)
        .build();
        api.set(js_string!(*name), callable, false, context)?;
    }

    context.register_global_property(
        js_string!("__h5i"),
        api,
        boa_engine::property::Attribute::empty(),
    )?;
    Ok(())
}

/// Restrict matches to descendants of `scope`, or leave them alone for the
/// document scope.
///
/// Blitz's selector engine always searches from the root, so `element.
/// querySelector` has to be narrowed here. Without this an app that scopes a
/// lookup to one panel would silently get a match from another, which is worse
/// than an error because it looks like it worked.
fn within(doc: &blitz_dom::BaseDocument, scope: usize, ids: Vec<usize>) -> Vec<usize> {
    if scope == 0 {
        return ids;
    }
    ids.into_iter()
        .filter(|id| {
            let mut current = *id;
            for _ in 0..256 {
                let Some(node) = doc.get_node(current) else {
                    return false;
                };
                let Some(parent) = node.parent else {
                    return false;
                };
                if parent == scope {
                    return true;
                }
                current = parent;
            }
            false
        })
        .collect()
}

// ── reading ────────────────────────────────────────────────────────────────

fn query(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let selector = arg_string(args, 0, context)?;
    let scope = arg_id(args, 1, context).unwrap_or(0);
    let host = host(context)?;
    let doc = host.dom.borrow();
    let all = doc
        .query_selector_all(&selector)
        .map(|found| found.to_vec())
        .unwrap_or_default();
    Ok(id_value(within(&doc, scope, all).into_iter().next()))
}

fn query_all(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let selector = arg_string(args, 0, context)?;
    let host = host(context)?;
    let scope = arg_id(args, 1, context).unwrap_or(0);
    let ids: Vec<usize> = {
        let doc = host.dom.borrow();
        let all = doc
            .query_selector_all(&selector)
            .map(|found| found.to_vec())
            .unwrap_or_default();
        within(&doc, scope, all)
    };
    let array = boa_engine::object::builtins::JsArray::new(context);
    for id in ids {
        array.push(JsValue::from(id as f64), context)?;
    }
    Ok(array.into())
}

fn get_text(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    Ok(match doc.get_node(id) {
        Some(node) => js_string!(node.text_content()).into(),
        None => JsValue::null(),
    })
}

fn get_attr(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let name = arg_string(args, 1, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    let found = doc.get_node(id).and_then(|node| {
        node.attrs().and_then(|attrs| {
            attrs
                .iter()
                .find(|a| a.name.local.as_ref() == name)
                .map(|a| a.value.to_string())
        })
    });
    Ok(match found {
        Some(value) => js_string!(value).into(),
        None => JsValue::null(),
    })
}

fn tag_name(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    Ok(match doc.get_node(id).and_then(|n| n.element_data()) {
        Some(el) => js_string!(el.name.local.as_ref().to_uppercase()).into(),
        None => JsValue::null(),
    })
}

fn is_element(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    Ok(JsValue::from(
        doc.get_node(id).is_some_and(|n| n.element_data().is_some()),
    ))
}

fn children(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let ids: Vec<usize> = {
        let doc = host.dom.borrow();
        doc.get_node(id).map(|n| n.children.clone()).unwrap_or_default()
    };
    let array = boa_engine::object::builtins::JsArray::new(context);
    for child in ids {
        array.push(JsValue::from(child as f64), context)?;
    }
    Ok(array.into())
}

fn parent(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    Ok(id_value(doc.get_node(id).and_then(|n| n.parent)))
}

fn root(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    let doc = host.dom.borrow();
    Ok(JsValue::from(doc.root_element().id as f64))
}

fn body(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    let doc = host.dom.borrow();
    Ok(id_value(doc.query_selector("body").ok().flatten()))
}

fn get_value(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    // The editor, not the `value` attribute: typing updates the former and
    // leaves the latter at whatever the HTML said. Same rule the snapshot uses.
    let text = doc
        .get_node(id)
        .and_then(|n| n.element_data())
        .and_then(|el| el.text_input_data())
        .map(|input| input.editor.text().to_string());
    Ok(match text {
        Some(text) => js_string!(text).into(),
        None => js_string!("").into(),
    })
}

// ── writing ────────────────────────────────────────────────────────────────

fn create_element(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let tag = arg_string(args, 0, context)?.to_lowercase();
    let host = host(context)?;
    let id = {
        let mut doc = host.dom.borrow_mut();
        let name = blitz_dom::QualName::new(
            None,
            blitz_dom::ns!(html),
            blitz_dom::LocalName::from(tag.as_str()),
        );
        let mut mutator = doc.mutate();
        mutator.create_element(name, Vec::new())
    };
    host.mark_dirty();
    Ok(JsValue::from(id as f64))
}

fn create_text(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = arg_string(args, 0, context)?;
    let host = host(context)?;
    let id = {
        let mut doc = host.dom.borrow_mut();
        let mut mutator = doc.mutate();
        mutator.create_text_node(&text)
    };
    host.mark_dirty();
    Ok(JsValue::from(id as f64))
}

fn append(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let parent_id = arg_id(args, 0, context)?;
    let child_id = arg_id(args, 1, context)?;
    let host = host(context)?;
    {
        let mut doc = host.dom.borrow_mut();
        let mut mutator = doc.mutate();
        mutator.append_children(parent_id, &[child_id]);
    }
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn insert_before(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let anchor = arg_id(args, 0, context)?;
    let new_id = arg_id(args, 1, context)?;
    let host = host(context)?;
    {
        let mut doc = host.dom.borrow_mut();
        let mut mutator = doc.mutate();
        mutator.insert_nodes_before(anchor, &[new_id]);
    }
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn remove_node(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    {
        let mut doc = host.dom.borrow_mut();
        let mut mutator = doc.mutate();
        mutator.remove_node(id);
    }
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn set_text(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let text = arg_string(args, 1, context)?;
    let host = host(context)?;
    {
        let mut doc = host.dom.borrow_mut();
        let mut mutator = doc.mutate();
        // `textContent = x` replaces the subtree, which is what a page expects
        // and what makes list rendering work at all.
        mutator.remove_and_drop_all_children(id);
        let text_id = mutator.create_text_node(&text);
        mutator.append_children(id, &[text_id]);
    }
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn set_attr(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let name = arg_string(args, 1, context)?.to_lowercase();
    let value = arg_string(args, 2, context)?;
    let host = host(context)?;
    {
        let mut doc = host.dom.borrow_mut();
        let qual = blitz_dom::QualName::new(
            None,
            blitz_dom::ns!(),
            blitz_dom::LocalName::from(name.as_str()),
        );
        let mut mutator = doc.mutate();
        mutator.set_attribute(id, qual, &value);
    }
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn remove_attr(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let name = arg_string(args, 1, context)?.to_lowercase();
    let host = host(context)?;
    {
        let mut doc = host.dom.borrow_mut();
        let qual = blitz_dom::QualName::new(
            None,
            blitz_dom::ns!(),
            blitz_dom::LocalName::from(name.as_str()),
        );
        let mut mutator = doc.mutate();
        mutator.clear_attribute(id, qual);
    }
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn set_inner_html(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let html = arg_string(args, 1, context)?;
    let host = host(context)?;
    {
        let mut doc = host.dom.borrow_mut();
        let mut mutator = doc.mutate();
        mutator.set_inner_html(id, &html);
    }
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn set_value(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let value = arg_string(args, 1, context)?;
    let host = host(context)?;
    {
        let mut doc = host.dom.borrow_mut();
        doc.with_text_input(id, |mut driver| {
            driver.select_all();
            driver.insert_or_replace_selection(&value);
        });
    }
    host.mark_dirty();
    Ok(JsValue::undefined())
}

// ── reporting ──────────────────────────────────────────────────────────────

fn log(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let level = arg_string(args, 0, context)?;
    let text = arg_string(args, 1, context)?;
    let host = host(context)?;
    host.console.borrow_mut().push(ConsoleLine { level, text });
    Ok(JsValue::undefined())
}

/// The page asked for something this engine does not have.
///
/// Recorded rather than thrown, and never silently stubbed. An agent has to be
/// able to tell "this page is empty" from "this page needed an API I lack",
/// and the count reaches the snapshot so it finds out where it is reading.
fn unsupported(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = arg_string(args, 0, context)?;
    let host = host(context)?;
    host.unsupported.borrow_mut().record(&name);
    Ok(JsValue::undefined())
}

/// `fetch`, routed through the same broker as everything else.
///
/// Synchronous underneath, and the prelude wraps the result in an
/// already-resolved promise. That is a real difference from a browser — there
/// is no concurrency, so two fetches run in order rather than at once — and it
/// is the honest trade for keeping the engine as the HTTP client. A page that
/// awaited them still observes the right order; a page that raced them for
/// speed does not get the speed.
///
/// The URL is recorded before the answer is returned, so a caller can correlate
/// the click that ran this script with the request it caused.
fn fetch(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg_string(args, 0, context)?;
    let method = arg_string(args, 1, context).unwrap_or_else(|_| "GET".to_string());
    let body = arg_string(args, 2, context).unwrap_or_default();
    let host = host(context)?;

    let resolved = match host.base.join(&target) {
        Ok(url) => url,
        Err(error) => return reply_error(&format!("`{target}` is not a URL: {error}"), context),
    };
    host.requests.borrow_mut().push(resolved.to_string());

    let content_type = (!body.is_empty()).then_some("application/x-www-form-urlencoded");
    // The document's own origin travels with the request, so the policy can
    // refuse a page from the web reaching the box's dev server (§3.1).
    let outcome = host.broker.send_from(
        &resolved,
        crate::receipt::Initiator::Subresource,
        &method,
        body.as_bytes(),
        content_type,
        Some(&host.base),
    );

    if let Some(error) = outcome.error {
        // A refusal is an answer. The promise rejects and the page sees it,
        // rather than the engine pretending the request never happened.
        return reply_error(&error, context);
    }

    let status = outcome.status.unwrap_or(0);
    let text = String::from_utf8_lossy(&outcome.body).into_owned();
    let reply = boa_engine::object::ObjectInitializer::new(context).build();
    reply.set(js_string!("ok"), (200..300).contains(&status), false, context)?;
    reply.set(js_string!("status"), status as f64, false, context)?;
    reply.set(js_string!("url"), js_string!(outcome.final_url.to_string()), false, context)?;
    reply.set(js_string!("text"), js_string!(text), false, context)?;

    let headers = boa_engine::object::builtins::JsArray::new(context);
    for (name, value) in &outcome.headers {
        let pair = boa_engine::object::builtins::JsArray::new(context);
        pair.push(JsValue::from(js_string!(name.as_str())), context)?;
        pair.push(JsValue::from(js_string!(value.as_str())), context)?;
        headers.push(pair, context)?;
    }
    reply.set(js_string!("headers"), headers, false, context)?;
    Ok(reply.into())
}

fn reply_error(message: &str, context: &mut Context) -> JsResult<JsValue> {
    let reply = boa_engine::object::ObjectInitializer::new(context).build();
    reply.set(js_string!("error"), js_string!(message), false, context)?;
    Ok(reply.into())
}


/// The serialised markup *inside* a node.
///
/// Real serialisation, not the text content. The previous version returned
/// `textContent`, which silently stripped every tag: a page doing
/// `el.innerHTML = el.innerHTML` destroyed its own subtree and nothing said so.
fn inner_html(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    let Some(node) = doc.get_node(id) else {
        return Ok(JsValue::null());
    };
    let mut out = String::new();
    for child in &node.children {
        if let Some(child) = doc.get_node(*child) {
            out.push_str(&child.outer_html());
        }
    }
    Ok(js_string!(out).into())
}

/// The serialised markup *of* a node, itself included.
fn outer_html(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    Ok(match doc.get_node(id) {
        Some(node) => js_string!(node.outer_html()).into(),
        None => JsValue::null(),
    })
}

/// A node's border box in viewport coordinates: `[x, y, width, height]`.
///
/// Blitz stores each box's position relative to its parent, so the absolute
/// position is the sum up the ancestor chain, minus the viewport scroll — which
/// is what makes this a *client* rect rather than a document one. Answered
/// rather than reported unsupported because the engine already computes it; the
/// previous version returned zeros, which sends a positioning library into a
/// loop that never converges.
fn rect(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();

    let Some(node) = doc.get_node(id) else {
        return Ok(JsValue::null());
    };
    let size = node.final_layout.size;

    let (mut x, mut y) = (0.0f32, 0.0f32);
    let mut current = Some(id);
    for _ in 0..256 {
        let Some(node_id) = current else { break };
        let Some(node) = doc.get_node(node_id) else { break };
        x += node.final_layout.location.x;
        y += node.final_layout.location.y;
        current = node.parent;
    }

    let scroll = doc.viewport_scroll();
    let array = boa_engine::object::builtins::JsArray::new(context);
    for value in [
        x as f64 - scroll.x,
        y as f64 - scroll.y,
        size.width as f64,
        size.height as f64,
    ] {
        array.push(JsValue::from(value), context)?;
    }
    Ok(array.into())
}

/// A curated set of computed values, read from the styles Stylo resolved.
///
/// Curated rather than complete because Stylo's per-property accessors are
/// generated at build time and there is no stable generic "give me property X
/// as a string" on `ComputedValues` to bind against. So this answers the
/// properties pages actually branch on — visibility checks and box metrics —
/// and anything else records itself as unsupported rather than returning a
/// plausible lie. A wrong `display` is worse than a missing one: it sends a
/// framework down a branch the real browser would never take.
fn computed_style(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let property = arg_string(args, 1, context)?.to_lowercase();
    let host = host(context)?;
    let doc = host.dom.borrow();

    let Some(node) = doc.get_node(id) else {
        return Ok(JsValue::null());
    };

    // Box metrics come from layout, which is resolved and therefore true.
    let layout = &node.final_layout;
    let answer = match property.as_str() {
        "width" => Some(format!("{}px", layout.size.width)),
        "height" => Some(format!("{}px", layout.size.height)),
        _ => None,
    };
    if let Some(answer) = answer {
        return Ok(js_string!(answer).into());
    }

    let Some(styles) = node.primary_styles() else {
        // No primary styles means the node is not rendered — `display: none`
        // is the honest answer for a visibility question about it.
        return Ok(match property.as_str() {
            "display" => js_string!("none").into(),
            _ => js_string!("").into(),
        });
    };

    use style_traits::ToCss as _;
    let answer = match property.as_str() {
        // `to_css_string`, not `{:?}`: Stylo's `Display` is a bitfield whose
        // Debug form is `display(514)`, and handing an agent that instead of
        // `block` is precisely the plausible lie this engine refuses.
        "display" => node.display_constructed_as.to_css_string(),
        "visibility" => styles.clone_visibility().to_css_string(),
        "position" => styles.clone_position().to_css_string(),
        "opacity" => styles.clone_opacity().to_string(),
        other => {
            host.unsupported
                .borrow_mut()
                .record(&format!("getComputedStyle({other})"));
            String::new()
        }
    };
    Ok(js_string!(answer).into())
}

/// Parse a URL against an optional base, using the same parser the broker uses.
///
/// Native rather than a JavaScript reimplementation because the engine already
/// contains a correct URL parser, and a second one in the prelude would
/// disagree with it about exactly the cases that matter — percent-encoding,
/// default ports, and what counts as an origin.
fn parse_url(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let href = arg_string(args, 0, context)?;
    let base = arg_string(args, 1, context).unwrap_or_default();

    let parsed = if base.is_empty() {
        url::Url::parse(&href)
    } else {
        url::Url::parse(&base).and_then(|base| base.join(&href))
    };

    let Ok(url) = parsed else {
        return Ok(JsValue::null());
    };

    let out = boa_engine::object::ObjectInitializer::new(context).build();
    let fields: [(&str, String); 8] = [
        ("href", url.to_string()),
        ("protocol", format!("{}:", url.scheme())),
        ("host", url.host_str().map(|h| match url.port() {
            Some(port) => format!("{h}:{port}"),
            None => h.to_string(),
        }).unwrap_or_default()),
        ("hostname", url.host_str().unwrap_or_default().to_string()),
        ("port", url.port().map(|p| p.to_string()).unwrap_or_default()),
        ("pathname", url.path().to_string()),
        ("search", url.query().map(|q| format!("?{q}")).unwrap_or_default()),
        ("hash", url.fragment().map(|f| format!("#{f}")).unwrap_or_default()),
    ];
    for (name, value) in fields {
        out.set(js_string!(name), js_string!(value), false, context)?;
    }
    out.set(
        js_string!("origin"),
        js_string!(url.origin().ascii_serialization()),
        false,
        context,
    )?;
    Ok(out.into())
}
