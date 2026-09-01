//! The native primitives, and nothing above them.

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

/// The node the prelude has no id for.
///
/// `document` is an object literal on the JavaScript side, not a wrapper, and it
/// deliberately carries no `_id`: every reflected accessor uses
/// `this._id === undefined` as its WebIDL brand check, so giving the document
/// one would make it pass for an element. Every path that hands `document._id`
/// to a primitive therefore hands over `undefined`, and every one of them means
/// this node.
const DOCUMENT_NODE_ID: usize = 0;

/// A node id argument, or an error naming what turned up instead.
fn bad_node_id(saw: &str) -> JsError {
    JsError::from_opaque(
        js_string!(format!(
            "a node id has to be a whole number, and this one is `{saw}`. That is \
             a bug in this engine, not in the page."
        ))
        .into(),
    )
}

fn arg_id(args: &[JsValue], index: usize, _context: &mut Context) -> JsResult<usize> {
    let value = args.get_or_undefined(index);
    if value.is_undefined() || value.is_null() {
        return Ok(DOCUMENT_NODE_ID);
    }
    // A *number*, not something a number can be made of. JavaScript's coercion
    // is happy to turn `[]` and `""` into 0, and 0 is the document, so a rule
    // written in terms of `to_number` would leave the same hole in a narrower
    // shape. Every id this side ever sees came from `this._id`, which is a
    // number or it is nothing.
    let Some(number) = value.as_number() else {
        return Err(bad_node_id(value.type_of()));
    };
    if !number.is_finite() || number.is_sign_negative() || number.fract() != 0.0 {
        return Err(bad_node_id(&number.to_string()));
    }
    Ok(number as usize)
}

/// Read a JS array of `[name, value]` pairs.
///
/// The shape `Headers` already iterates in, so the prelude hands over what it
/// has rather than a second representation that could disagree with the first.
fn string_pairs(args: &[JsValue], index: usize, context: &mut Context) -> Vec<(String, String)> {
    let Some(object) = args.get(index).and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let Ok(array) = boa_engine::object::builtins::JsArray::from_object(object.clone()) else {
        return Vec::new();
    };
    let length = array.length(context).unwrap_or(0);
    let mut out = Vec::new();
    for at in 0..length {
        let Ok(entry) = array.get(at, context) else {
            continue;
        };
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Ok(pair) = boa_engine::object::builtins::JsArray::from_object(entry.clone()) else {
            continue;
        };
        let name = pair
            .get(0u64, context)
            .ok()
            .and_then(|v| v.to_string(context).ok())
            .map(|v| v.to_std_string_escaped())
            .unwrap_or_default();
        let value = pair
            .get(1u64, context)
            .ok()
            .and_then(|v| v.to_string(context).ok())
            .map(|v| v.to_std_string_escaped())
            .unwrap_or_default();
        if !name.is_empty() {
            out.push((name, value));
        }
    }
    out
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
        ("fetchStart", 6, fetch_start),
        ("fetchDrain", 0, fetch_drain),
        ("fetchPending", 0, fetch_pending),
        ("userAgent", 0, user_agent),
        #[cfg(feature = "identity")]
        ("identity", 0, identity),
        ("attrNames", 1, attr_names),
        ("nodeKind", 1, node_kind),
        ("isConnected", 1, is_connected),
        ("randomBytes", 1, random_bytes),
        ("matchesSelector", 2, matches_selector),
        ("elementFromPoint", 2, element_from_point),
        ("innerHtml", 1, inner_html),
        ("outerHtml", 1, outer_html),
        ("rect", 1, rect),
        ("canvasOp", 3, canvas_op),
        ("canvasSize", 4, canvas_size),
        ("canvasPng", 1, canvas_png),
        ("computedStyle", 2, computed_style),
        ("supportsCss", 2, supports_css),
        ("validSelector", 1, valid_selector),
        ("documentEncoding", 0, document_encoding),
        ("isCssProperty", 1, is_css_property),
        ("innerText", 1, inner_text),
        ("encodingFor", 1, encoding_for),
        ("decodeBytes", 3, decode_bytes),
        ("parseUrl", 2, parse_url),
        ("serializeCssValue", 2, serialize_css_value),
        ("urlWithUserinfo", 3, url_with_userinfo),
        ("viewport", 0, viewport),
        ("readCookies", 0, read_cookies),
        ("writeCookie", 1, write_cookie),
        ("scrollToNode", 1, scroll_to_node),
        ("createComment", 1, create_comment),
        ("scrollMetrics", 1, scroll_metrics),
        ("setScrollTop", 2, set_scroll_top),
        ("socketOpen", 1, socket_open),
        ("socketSend", 2, socket_send),
        ("socketClose", 1, socket_close),
        ("socketDrain", 1, socket_drain),
        ("sseOpen", 1, sse_open),
        ("sseClose", 1, sse_close),
        ("sseDrain", 1, sse_drain),
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

// ── reading ────────────────────────────────────────────────────────────────

fn query(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let selector = arg_string(args, 0, context)?;
    let scope = arg_id(args, 1, context).unwrap_or(0);
    let host = host(context)?;
    let doc = host.dom.borrow();
    Ok(id_value(
        matches_within(&doc, scope, &selector).into_iter().next(),
    ))
}

fn query_all(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let selector = arg_string(args, 0, context)?;
    let host = host(context)?;
    let scope = arg_id(args, 1, context).unwrap_or(0);
    let ids: Vec<usize> = {
        let doc = host.dom.borrow();
        matches_within(&doc, scope, &selector)
    };
    let array = boa_engine::object::builtins::JsArray::new(context)?;
    for id in ids {
        array.push(JsValue::from(id as f64), context)?;
    }
    Ok(array.into())
}

/// Every node under `scope` matching `selector`, in document order.
pub(crate) fn matches_within(
    doc: &blitz_dom::BaseDocument,
    scope: usize,
    selector: &str,
) -> Vec<usize> {
    let Ok(list) = doc.try_parse_selector_list(selector) else {
        return Vec::new();
    };

    if scope == 0 {
        let mut found = smallvec::SmallVec::<[&blitz_dom::Node; 128]>::new();
        style::dom_apis::query_selector::<&blitz_dom::Node, style::dom_apis::QueryAll>(
            doc.root_node(),
            &list,
            &mut found,
            style::dom_apis::MayUseInvalidation::Yes,
        );
        return found.into_iter().map(|node| node.id).collect();
    }

    let mut out = Vec::new();
    collect_matches(doc, scope, &list, &mut out);
    out
}

/// Depth-first over the descendants of `id`, in document order.
///
/// The scope itself is deliberately not tested: `element.querySelector` is
/// defined to search inside the element, not to return it.
fn collect_matches(
    doc: &blitz_dom::BaseDocument,
    id: usize,
    list: &selectors::SelectorList<style::selector_parser::SelectorImpl>,
    out: &mut Vec<usize>,
) {
    let Some(node) = doc.get_node(id) else { return };
    for child in node.children.clone() {
        if let Some(child_node) = doc.get_node(child)
            && child_node.is_element()
                // Standards mode: this engine parses HTML5 and never emulates
                // the quirks the flag exists to preserve.
                && style::dom_apis::element_matches(
                    &child_node,
                    list,
                    style::context::QuirksMode::NoQuirks,
                )

        {
            out.push(child);
        }
        collect_matches(doc, child, list, out);
    }
}

fn get_text(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    // A comment's text lives beside the tree, so it is answered before the tree
    // is consulted at all.
    if let Some(text) = host.comments.borrow().get(&id) {
        return Ok(JsValue::from(js_string!(text.as_str())));
    }
    let doc = host.dom.borrow();
    Ok(match doc.get_node(id) {
        Some(node) => js_string!(node.text_content()).into(),
        None => JsValue::null(),
    })
}

/// The qualified name an attribute lookup should actually use.
fn attr_name_for<'a>(
    doc: &blitz_dom::BaseDocument,
    id: usize,
    name: &'a str,
) -> std::borrow::Cow<'a, str> {
    let html_element = doc
        .get_node(id)
        .and_then(|node| node.element_data())
        .is_some_and(|el| el.name.ns == blitz_dom::ns!(html));
    if html_element && name.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(name.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(name)
    }
}

fn get_attr(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let name = arg_string(args, 1, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    let name = attr_name_for(&doc, id, &name);
    // Straight to a `JsString` from the borrowed value: the intermediate
    // `String` this used to build was the third allocation in a read that only
    // ever needed one.
    let found = doc.get_node(id).and_then(|node| {
        node.attrs().and_then(|attrs| {
            attrs
                .iter()
                .find(|a| a.name.local.as_ref() == name.as_ref())
                .map(|a| js_string!(a.value.as_str()))
        })
    });
    Ok(match found {
        Some(value) => value.into(),
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
    let array = boa_engine::object::builtins::JsArray::new(context)?;
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
    // `null`, not `""`, when there is no editor at all.
    //
    // Blitz builds editor state only for a control it has laid out, so a
    // detached `<input>` (and a `<textarea>`, whose value is its text content)
    // have none. Answering `""` for both cases made "this field is empty"
    // and "I cannot see this field" the same answer, and the prelude could not
    // tell which it had. It now falls back to the markup for the second.
    Ok(match text {
        Some(text) => js_string!(text).into(),
        None => JsValue::null(),
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

/// A real comment node, because a marker that is secretly a text node shows up
/// in `textContent` and in the outline an agent reads.
///
/// Template libraries anchor themselves to comments. An empty list leaves
/// behind `<!--list-->` and the library finds its place again by it.
fn create_comment(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = arg_string(args, 0, context)?;
    let host = host(context)?;
    let id = {
        let mut doc = host.dom.borrow_mut();
        let id = doc.create_node(blitz_dom::NodeData::Comment);
        // The data is kept beside the node rather than in it: `NodeData::Comment`
        // carries no text in this version of blitz, and a page that writes a
        // comment and reads it back should get what it wrote.
        host.comments.borrow_mut().insert(id, text);
        id
    };
    host.mark_dirty();
    Ok(JsValue::from(id as f64))
}

/// `scrollTop`/`scrollHeight` and their siblings, in one call.
///
/// Six values rather than six bindings because a page asking for one almost
/// always asks for the next in the same expression: `el.scrollTop + el.clientHeight
/// >= el.scrollHeight` is how every "am I at the bottom" check is written.
fn scroll_metrics(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    settle_layout(&host);
    let doc = host.dom.borrow();

    let Some(node) = doc.get_node(id) else {
        return Ok(JsValue::null());
    };

    // The document scrolls; an ordinary element in this engine does not, because
    // nothing here clips and scrolls a subtree. Saying so plainly beats
    // reporting a scrollTop that can never change.
    let (view_w, view_h) = doc.viewport().window_size;
    let values: [f64; 6] = if is_document_scroller(&doc, id) {
        let scroll = doc.viewport_scroll();
        let content = document_height(&doc);
        [
            scroll.y,
            scroll.x,
            content.max(view_h as f64),
            view_w as f64,
            view_h as f64,
            view_w as f64,
        ]
    } else {
        // `client*` is the box; `scroll*` is the box or its overflow, whichever
        // is larger. Collapsing the two would make the bottom-check above read
        // "already at the bottom" for every element that has more inside it.
        let box_height = node.final_layout.size.height as f64;
        let box_width = node.final_layout.size.width as f64;
        [
            0.0,
            0.0,
            element_height(node),
            box_width.max(node.final_layout.content_size.width as f64),
            box_height,
            box_width,
        ]
    };

    let array = boa_engine::object::builtins::JsArray::new(context)?;
    for value in values {
        array.push(JsValue::from(value), context)?;
    }
    Ok(array.into())
}

/// `documentElement` and `body` both stand for the page. Pages read scroll
/// position off whichever one they were taught.
fn is_document_scroller(doc: &blitz_dom::BaseDocument, id: usize) -> bool {
    if id == doc.root_element().id {
        return true;
    }
    doc.query_selector_all("body")
        .ok()
        .and_then(|ids| ids.first().copied())
        == Some(id)
}

/// How tall the document actually is.
///
/// `size.height` alone reads as one screen for a page whose root box simply
/// grew past the viewport. The same trap `Page::max_scroll_y` documents, and
/// the reason a naive `scrollHeight` reported a 4000px page as unscrollable.
fn document_height(doc: &blitz_dom::BaseDocument) -> f64 {
    let layout = &doc.root_element().final_layout;
    layout.size.height.max(layout.content_size.height) as f64
}

fn element_height(node: &blitz_dom::Node) -> f64 {
    node.final_layout
        .size
        .height
        .max(node.final_layout.content_size.height) as f64
}

/// Scroll the document to an absolute offset. Backs `documentElement.scrollTop = y`.
fn set_scroll_top(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let _id = arg_id(args, 0, context)?;
    let y = args
        .get(1)
        .unwrap_or(&JsValue::undefined())
        .to_number(context)?;
    let host = host(context)?;
    let mut doc = host.dom.borrow_mut();

    let view_h = doc.viewport().window_size.1 as f64;
    let max = (document_height(&doc) - view_h).max(0.0);
    let x = doc.viewport_scroll().x;

    doc.set_viewport_scroll(blitz_dom::Point {
        x,
        y: y.clamp(0.0, max),
    });
    Ok(JsValue::undefined())
}

/// Would putting `child` inside `parent` make the tree cyclic?
fn would_cycle(doc: &blitz_dom::BaseDocument, parent: usize, child: usize) -> bool {
    let mut at = Some(parent);
    while let Some(id) = at {
        if id == child {
            return true;
        }
        at = doc.get_node(id).and_then(|node| node.parent);
    }
    false
}

/// The error a refused insertion reports, in the spec's own terms.
fn hierarchy_request(what: &str) -> JsError {
    JsError::from_opaque(
        js_string!(format!(
            "HierarchyRequestError: {what} would make a node contain itself"
        ))
        .into(),
    )
}

fn append(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let parent_id = arg_id(args, 0, context)?;
    let child_id = arg_id(args, 1, context)?;
    let host = host(context)?;
    if would_cycle(&host.dom.borrow(), parent_id, child_id) {
        return Err(hierarchy_request("appending a node"));
    }
    guard_mutation(&host, "appending a node", || {
        let mut doc = host.dom.borrow_mut();
        let mut mutator = doc.mutate();
        mutator.append_children(parent_id, &[child_id]);
    });
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn insert_before(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let anchor = arg_id(args, 0, context)?;
    let new_id = arg_id(args, 1, context)?;
    let host = host(context)?;
    {
        // A read is all the parent check needs; the mutation takes its own
        // borrow inside the guard below.
        let doc = host.dom.borrow();
        // blitz inserts relative to the anchor's parent and unwraps it, so an
        // anchor with no parent aborts the process. Checked here rather than
        // only in the prelude because a panic is not a DOM error: it takes the
        // page, the snapshot and the receipts with it, and WPT reaches this
        // path on purpose (`ChildNode-after`, `-before`, `-replaceWith`).
        // The same cycle rule `append` carries, at the other door into the tree:
        // the new node must not already contain the anchor it is going before.
        if let Some(parent) = doc.get_node(anchor).and_then(|node| node.parent)
            && would_cycle(&doc, parent, new_id)
        {
            return Err(hierarchy_request("inserting a node"));
        }
        if doc.get_node(anchor).map(|node| node.parent.is_none()).unwrap_or(true) {
            return Err(boa_engine::JsNativeError::error()
                .with_message(
                    "insertBefore: the reference node has no parent, so there is \
                     nowhere to insert before it",
                )
                .into());
        }
    }
    guard_mutation(&host, "inserting a node", || {
        let mut doc = host.dom.borrow_mut();
        let mut mutator = doc.mutate();
        mutator.insert_nodes_before(anchor, &[new_id]);
    });
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn remove_node(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    // `Mutator::remove_node` indexes the node arena unchecked, so a stale id
    // panics rather than returning. Removing a node that is already gone is an
    // ordinary thing for a page to do, it is what every "remove it if it is
    // still there" teardown looks like, and it must be a quiet no-op, not a
    // panic caught into a false success.
    if host.dom.borrow().get_node(id).is_none() {
        return Ok(JsValue::undefined());
    }
    guard_mutation(&host, "removing a node", || {
        let mut doc = host.dom.borrow_mut();
        let mut mutator = doc.mutate();
        mutator.remove_node(id);
    });
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn set_text(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let text = arg_string(args, 1, context)?;
    let host = host(context)?;

    // A comment keeps its text beside the tree, because `NodeData::Comment`
    // carries none.
    if host.comments.borrow().contains_key(&id) {
        host.comments.borrow_mut().insert(id, text);
        host.mark_dirty();
        return Ok(JsValue::undefined());
    }

    guard_mutation(&host, "setting text", || {
        let mut doc = host.dom.borrow_mut();
        // Writing to a *text* node replaces its own text. Writing to an element
        // replaces its subtree. They were both taking the element path, so a
        // text node had its (nonexistent) children cleared and a text child
        // appended to it, and the write simply vanished.
        //
        // Text nodes were therefore immutable, which is the single most common
        // mutation any reactive UI performs: every framework updates text by
        // assigning `.data` or `.nodeValue` on the node it already has.
        let is_text = matches!(
            doc.get_node(id).map(|node| &node.data),
            Some(blitz_dom::NodeData::Text(_))
        );

        // Detach the old children; do not destroy them.
        let old: Vec<usize> = doc
            .get_node(id)
            .map(|node| node.children.clone())
            .unwrap_or_default();
        let mut mutator = doc.mutate();
        if is_text {
            mutator.set_node_text(id, &text);
        } else {
            for child in old {
                mutator.remove_node(child);
            }
            let text_id = mutator.create_text_node(&text);
            mutator.append_children(id, &[text_id]);
        }
    });
    host.mark_dirty();
    Ok(JsValue::undefined())
}

/// Ask the next restyle to re-match the whole tree, not just the changed element and its
/// parent.
fn hint_whole_document_restyle(doc: &mut blitz_dom::BaseDocument) {
    use style::invalidation::element::restyle_hints::RestyleHint;
    let root_id = doc.root_element().id;
    if let Some(node) = doc.get_node_mut(root_id)
        && let Some(mut data) = node.stylo_element_data.get_mut()
    {
        data.hint.insert(RestyleHint::restyle_subtree());
    }
}

fn set_attr(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let raw = arg_string(args, 1, context)?;
    let value = arg_string(args, 2, context)?;
    let host = host(context)?;
    // Same rule as the read path, and it has to be the same rule: a writer that
    // lowercases where the reader does not is how `accessKey` went missing.
    let name = attr_name_for(&host.dom.borrow(), id, &raw);
    guard_mutation(&host, &format!("setting `{name}`"), || {
        let mut doc = host.dom.borrow_mut();
        let qual = blitz_dom::QualName::new(
            None,
            blitz_dom::ns!(),
            blitz_dom::LocalName::from(&*name),
        );
        let mut mutator = doc.mutate();
        mutator.set_attribute(id, qual, &value);
        drop(mutator);
        hint_whole_document_restyle(&mut doc);
    });
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn remove_attr(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let raw = arg_string(args, 1, context)?;
    let host = host(context)?;
    let name = attr_name_for(&host.dom.borrow(), id, &raw);
    {
        let mut doc = host.dom.borrow_mut();
        let qual = blitz_dom::QualName::new(
            None,
            blitz_dom::ns!(),
            blitz_dom::LocalName::from(&*name),
        );
        let mut mutator = doc.mutate();
        mutator.clear_attribute(id, qual);
        drop(mutator);
        hint_whole_document_restyle(&mut doc);
    }
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn set_inner_html(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let html = arg_string(args, 1, context)?;
    let host = host(context)?;
    // Refuse non-element targets *here*, because blitz's fragment parser
    // panics on them. Reachable from a page: borrow the `innerHTML` setter
    // off `Element.prototype` and `.call` it on a doctype wrapper, or just
    // `Object.assign` one wrapper onto another now that members are
    // enumerable, and one line of page script took the whole engine down.
    {
        let doc = host.dom.borrow();
        let is_element = doc.get_node(id).map(|n| n.is_element()).unwrap_or(false);
        if !is_element {
            return Err(boa_engine::JsNativeError::typ()
                .with_message("innerHTML: the target is not an element")
                .into());
        }
    }
    guard_mutation(&host, "setting innerHTML", || {
        let mut doc = host.dom.borrow_mut();
        let mut mutator = doc.mutate();
        mutator.set_inner_html(id, &html);
    });
    host.mark_dirty();
    Ok(JsValue::undefined())
}

fn set_value(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let value = arg_string(args, 1, context)?;
    let host = host(context)?;
    let reached_editor = {
        let mut doc = host.dom.borrow_mut();
        let has_editor = doc
            .get_node(id)
            .and_then(|n| n.element_data())
            .and_then(|el| el.text_input_data())
            .is_some();
        if has_editor {
            doc.with_text_input(id, |mut driver| {
                driver.select_all();
                driver.insert_or_replace_selection(&value);
            });
        }
        has_editor
    };
    host.mark_dirty();
    // Whether the write landed, so the prelude knows if it has to remember the
    // value itself. A detached control has no editor to write into, and
    // silently dropping the assignment meant `el.value = "y"` read back as "".
    // A page that filled in a form it had just built saw none of it.
    Ok(JsValue::from(reached_editor))
}

// ── reporting ──────────────────────────────────────────────────────────────

fn log(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let level = arg_string(args, 0, context)?;
    let text = arg_string(args, 1, context)?;
    let host = host(context)?;
    crate::script::host::push_console(
        &mut host.console.borrow_mut(),
        ConsoleLine::page(&level, text),
    );
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
fn element_from_point(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let x = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_number(context)? as f32;
    let y = args
        .get(1)
        .unwrap_or(&JsValue::undefined())
        .to_number(context)? as f32;
    let host = host(context)?;
    settle_layout(&host);
    let doc = host.dom.borrow();

    let scroll = doc.viewport_scroll();
    let Some(hit) = doc.hit(x + scroll.x as f32, y + scroll.y as f32) else {
        return Ok(JsValue::null());
    };

    let mut id = hit.node_id;
    for _ in 0..16 {
        match doc.get_node(id) {
            Some(node) if node.is_element() => return Ok(JsValue::from(id as f64)),
            Some(node) => match node.parent {
                Some(parent) => id = parent,
                None => break,
            },
            None => break,
        }
    }
    Ok(JsValue::null())
}

/// Does this one element match the selector?
///
/// A direct predicate on the element, no traversal at all. An earlier version
/// asked the *parent* for all its matching descendants and checked membership,
/// which made every `matches()` call walk a subtree and every `closest()` walk
/// one per ancestor. On a page whose framework calls `closest` in a render loop
/// that is quadratic, and it took a real site from seconds to minutes.
fn matches_selector(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let selector = arg_string(args, 1, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();

    let Ok(list) = doc.try_parse_selector_list(&selector) else {
        return Ok(JsValue::from(false));
    };
    let Some(node) = doc.get_node(id) else {
        return Ok(JsValue::from(false));
    };
    if !node.is_element() {
        return Ok(JsValue::from(false));
    }

    Ok(JsValue::from(style::dom_apis::element_matches(
        &node,
        &list,
        style::context::QuirksMode::NoQuirks,
    )))
}

/// Bytes from the operating system's CSPRNG, for `crypto.getRandomValues`.
///
/// The real thing rather than a seeded generator. A page cannot tell the
/// difference, which is exactly why substituting one would be a lie: the
/// property `crypto` names is unpredictability, and a nonce or an id built on a
/// predictable stream is broken in a way nothing here would surface.
fn random_bytes(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let count = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_number(context)? as usize;
    // Bounded so a page cannot ask this process for an arbitrary allocation.
    // A browser refuses above 65536 for the same reason.
    if count > 65_536 {
        return Err(boa_engine::JsNativeError::typ()
            .with_message("getRandomValues: at most 65536 bytes at a time")
            .into());
    }

    let mut bytes = vec![0u8; count];
    if let Err(error) = getrandom::getrandom(&mut bytes) {
        return Err(boa_engine::JsNativeError::error()
            .with_message(format!("the system random source refused: {error}"))
            .into());
    }

    let out = boa_engine::object::builtins::JsArray::new(context)?;
    for byte in bytes {
        out.push(JsValue::from(byte as f64), context)?;
    }
    Ok(out.into())
}

/// Is this node attached to the document?
///
/// One call instead of one per ancestor. The JavaScript version asked `parent`
/// in a loop, so the cost of an `appendChild` grew with how deep the page was,
/// and every insertion pays it, because a node has to be connected before its
/// `connectedCallback` can run.
fn is_connected(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();

    let root = doc.root_element().id;
    let mut current = Some(id);
    // Bounded, because a corrupt tree must not become an infinite loop here.
    for _ in 0..4096 {
        let Some(node_id) = current else { break };
        if node_id == root {
            return Ok(JsValue::from(true));
        }
        current = doc.get_node(node_id).and_then(|node| node.parent);
    }
    Ok(JsValue::from(false))
}

/// What kind of node this is, in DOM numbering.
///
/// Asked of the tree rather than remembered on the JavaScript side. A set of
/// ids populated by `createComment` only knows about comments *script* made,
/// so every comment the parser produced came back as a text node, and preact
/// and React both separate adjacent text with `<!-- -->` when they render on
/// the server. Hydration saw text where it expected a comment, decided the
/// markup did not match, and rendered the page a second time beside the first.
fn node_kind(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();

    let kind = match doc.get_node(id).map(|node| &node.data) {
        Some(blitz_dom::NodeData::Element(_)) | Some(blitz_dom::NodeData::AnonymousBlock(_)) => 1,
        Some(blitz_dom::NodeData::Text(_)) => 3,
        Some(blitz_dom::NodeData::Comment) => 8,
        Some(blitz_dom::NodeData::Document) => 9,
        _ => 0,
    };
    Ok(JsValue::from(kind as f64))
}

/// Every attribute on an element, in source order.
///
/// Backs `Element.attributes` and `getAttributeNames`. Names only: the values
/// come back through `getAttr`, so there is one place that reads them.
fn attr_names(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();

    let names: Vec<String> = doc
        .get_node(id)
        .and_then(|node| node.attrs())
        .map(|attrs| {
            attrs
                .iter()
                .map(|a| a.name.local.as_ref().to_string())
                .collect()
        })
        .unwrap_or_default();

    let out = boa_engine::object::builtins::JsArray::new(context)?;
    for name in names {
        out.push(JsValue::from(js_string!(name.as_str())), context)?;
    }
    Ok(out.into())
}

/// The session's agent string, so the prelude cannot hold a second copy that
/// drifts from the one the broker put on the wire.
#[cfg(feature = "identity")]
fn user_agent(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    Ok(JsValue::from(js_string!(
        host.identity.browser.user_agent.as_str()
    )))
}

/// The same, for a build with no identities: there is one agent string, and it
/// is a constant.
#[cfg(not(feature = "identity"))]
fn user_agent(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(js_string!(crate::net::USER_AGENT)))
}

/// Everything else the session's identity declares, as one plain object.
#[cfg(feature = "identity")]
fn identity(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    let identity = &host.identity;

    fn put(
        object: &boa_engine::object::JsObject,
        name: &str,
        value: JsValue,
        context: &mut Context,
    ) -> JsResult<()> {
        object.set(js_string!(name), value, false, context)?;
        Ok(())
    }

    let out = boa_engine::object::JsObject::with_null_proto();
    for (name, text) in [
        ("mode", identity.mode.as_str()),
        ("platform", identity.device.platform.as_str()),
        ("vendor", identity.browser.vendor.as_str()),
        ("productSub", identity.browser.product_sub.as_str()),
        ("oscpu", identity.device.oscpu.as_str()),
    ] {
        put(&out, name, js_string!(text).into(), context)?;
    }
    for (name, number) in [
        ("hardwareConcurrency", identity.device.hardware_concurrency),
        ("maxTouchPoints", identity.device.max_touch_points),
    ] {
        put(&out, name, JsValue::from(number), context)?;
    }

    let languages = boa_engine::object::builtins::JsArray::new(context)?;
    for tag in &identity.locale.languages {
        languages.push(JsValue::from(js_string!(tag.as_str())), context)?;
    }
    put(&out, "languages", languages.into(), context)?;

    // Absent rather than null when the identity declares no display. The rule
    // `prelude.js` already follows is that a name which exists and answers
    // wrongly is worse than one that is absent, and a headless engine's honest
    // screen size is a guess, so `native` and `privacy` expose no `screen` at
    // all, exactly as before this module existed. A *declared* identity is the
    // case that rule was waiting for: the answer is stated, not guessed.
    if let Some(screen) = &identity.screen {
        let object = boa_engine::object::JsObject::with_null_proto();
        for (name, value) in [
            ("width", screen.width),
            ("height", screen.height),
            ("availWidth", screen.avail_width),
            ("availHeight", screen.avail_height),
            ("colorDepth", screen.color_depth),
        ] {
            put(&object, name, JsValue::from(value), context)?;
        }
        put(
            &object,
            "devicePixelRatio",
            JsValue::from(screen.device_pixel_ratio()),
            context,
        )?;
        put(&out, "screen", object.into(), context)?;
    }

    Ok(out.into())
}

/// Accept a request from script and hand back a ticket.
///
/// Nothing goes on the wire here. The request joins an ordered queue, and
/// [`fetch_drain`] starts it when a slot is free, which is what makes two
/// `fetch` calls actually overlap instead of running one after the other. The
/// old binding did the whole round trip inline, so a page that fanned out ten
/// requests paid for them in series and every SPA waterfall was our own.
fn fetch_start(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg_string(args, 0, context)?;
    let method = arg_string(args, 1, context).unwrap_or_else(|_| "GET".to_string());
    let body = arg_string(args, 2, context).unwrap_or_default();
    // The rest of the request's origin story: what the page set, and how it
    // asked to treat the boundary. Defaults match `fetch`'s own (`cors` mode,
    // `same-origin` credentials) so a page that says nothing gets the
    // behaviour a browser gives it rather than the widest one available.
    let mode = crate::cors::Mode::parse(&arg_string(args, 3, context).unwrap_or_default());
    let credentials =
        crate::cors::Credentials::parse(&arg_string(args, 4, context).unwrap_or_default());
    let headers = string_pairs(args, 5, context);
    let host = host(context)?;

    let resolved = match host.base.join(&target) {
        Ok(url) => url,
        Err(error) => return reply_error(&format!("`{target}` is not a URL: {error}"), context),
    };
    // The queue, before the ticket. `fetch()` returns before anything is
    // decided about the request, so a loop calling it builds a slot per call
    // (a URL, a method, a body, headers) and the drain runs once per settle
    // round. The request budget refuses requests; it never saw the queue.
    if host.pending_fetches.borrow().len() >= crate::script::host::MAX_QUEUED_FETCHES {
        return reply_error(
            &format!(
                "this page has {} requests queued and not yet started, which is the most one \
                 page may hold. The per-navigation request budget is the bound on how many it \
                 may make; this is the bound on how many it may have waiting.",
                crate::script::host::MAX_QUEUED_FETCHES
            ),
            context,
        );
    }

    let id = host.next_fetch.get();
    host.next_fetch.set(id + 1);
    {
        // Bounded like the queue, and for the same reason: this list is the
        // causal join between an action and a row in the request log, and it
        // grew one entry per `fetch()` for the life of the page. What is
        // dropped is the *link*; the receipt is the broker's and is untouched.
        let mut links = host.requests.borrow_mut();
        if links.len() < crate::script::host::MAX_REQUEST_LINKS {
            links.push(crate::script::host::RequestLink {
                ticket: id,
                url: resolved.to_string(),
                seq: None,
            });
        }
    }
    host.pending_fetches.borrow_mut().insert(
        id,
        crate::script::host::FetchSlot::Queued {
            url: resolved,
            method,
            // Whatever the page set, or the default `fetch` applies to a body.
            // Read from the headers rather than assumed, because the value
            // decides whether the request preflights: `application/json` is
            // deliberately not on the CORS safelist.
            content_type: headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                .map(|(_, value)| value.clone())
                .or_else(|| {
                    (!body.is_empty())
                        .then(|| "application/x-www-form-urlencoded".to_string())
                }),
            body: body.into_bytes(),
            headers,
            mode,
            credentials,
        },
    );
    Ok(JsValue::from(id as f64))
}

/// Start what can be started, and return whatever has come back.
///
/// Called from the settle loop, so the page's promises resolve as the network
/// answers rather than at some arbitrary later point.
fn fetch_drain(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;

    // 1. Fill the free slots, in the order the page asked.
    {
        let mut pending = host.pending_fetches.borrow_mut();
        let mut in_flight = pending
            .values()
            .filter(|slot| matches!(slot, crate::script::host::FetchSlot::InFlight(_)))
            .count();

        let startable: Vec<u64> = pending
            .iter()
            .filter(|(_, slot)| matches!(slot, crate::script::host::FetchSlot::Queued { .. }))
            .map(|(id, _)| *id)
            .collect();

        for id in startable {
            if in_flight >= crate::script::host::MAX_INFLIGHT_FETCHES {
                break;
            }
            let Some(crate::script::host::FetchSlot::Queued {
                url,
                method,
                body,
                content_type,
                headers,
                mode,
                credentials,
            }) = pending.remove(&id)
            else {
                continue;
            };

            let (tx, rx) = std::sync::mpsc::channel();
            let broker = host.broker.clone();
            // Kept for the error path, which needs to name the request that
            // could not be started after the closure has taken the original.
            let named = url.clone();
            // The document's own origin travels with the request, so the policy
            // can refuse a page from the web reaching the box's dev server
            // (§3.1). It is cloned rather than borrowed because this leaves the
            // thread that owns the realm.
            let document = host.base.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("h5i-fetch-{id}"))
                .spawn(move || {
                    // `send_script`, not `send_from`: a page exercises an
                    // authority it has to be granted, and the broker has to be
                    // told whose authority it is before it can decide.
                    let outcome = broker.send_script(
                        &url,
                        &method,
                        &body,
                        content_type.as_deref(),
                        &document,
                        &headers,
                        mode,
                        credentials,
                    );
                    // A closed receiver means the realm went away; there is
                    // nobody left to tell.
                    let _ = tx.send(outcome);
                });

            match spawned {
                Ok(_) => {
                    pending.insert(id, crate::script::host::FetchSlot::InFlight(rx));
                    in_flight += 1;
                }
                Err(error) => {
                    // Out of threads is a real answer, not a hang.
                    let (tx, rx) = std::sync::mpsc::channel();
                    let _ = tx.send(crate::net::FetchOutcome::refused(
                        named,
                        format!("could not start the request: {error}"),
                    ));
                    pending.insert(id, crate::script::host::FetchSlot::InFlight(rx));
                }
            }
        }
    }

    // 2. Collect what has arrived. Taken out of the map first so the borrow is
    //    released before any JS object is built.
    let mut arrived: Vec<(u64, crate::net::FetchOutcome)> = Vec::new();
    {
        let mut pending = host.pending_fetches.borrow_mut();
        // Exactly one `try_recv` per slot. Asking twice, once to find out
        // whether an answer was there and again to take it, takes the value on
        // the first call and finds an empty channel on the second, which read
        // as every request ending without an answer.
        let ids: Vec<u64> = pending.keys().copied().collect();
        for id in ids {
            let taken = match pending.get(&id) {
                Some(crate::script::host::FetchSlot::InFlight(rx)) => match rx.try_recv() {
                    Ok(outcome) => Some(outcome),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    // The worker died without sending: report it rather than
                    // leaving the page's promise pending forever.
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Some(crate::net::FetchOutcome::refused(
                            host.base.clone(),
                            "the request ended without an answer".to_string(),
                        ))
                    }
                },
                _ => None,
            };
            if let Some(outcome) = taken {
                pending.remove(&id);
                // Now the receipt exists, so this request can name it. That
                // number is what lets the console draw "this click, this row".
                if let Some(link) = host
                    .requests
                    .borrow_mut()
                    .iter_mut()
                    .find(|link| link.ticket == id)
                {
                    link.seq = outcome.seq;
                }
                arrived.push((id, outcome));
            }
        }
    }

    let out = boa_engine::object::builtins::JsArray::new(context)?;
    for (id, outcome) in arrived {
        let pair = boa_engine::object::builtins::JsArray::new(context)?;
        pair.push(JsValue::from(id as f64), context)?;
        pair.push(reply_value(outcome, context)?, context)?;
        out.push(pair, context)?;
    }
    Ok(out.into())
}

/// How many requests are still owed an answer, so `settle` knows to wait.
fn fetch_pending(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    let count = host.pending_fetches.borrow().len();
    Ok(JsValue::from(count as f64))
}

/// The shape script sees for one finished request.
fn reply_value(
    outcome: crate::net::FetchOutcome,
    context: &mut Context,
) -> JsResult<JsValue> {
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

    let headers = boa_engine::object::builtins::JsArray::new(context)?;
    for (name, value) in &outcome.headers {
        let pair = boa_engine::object::builtins::JsArray::new(context)?;
        pair.push(JsValue::from(js_string!(name.as_str())), context)?;
        pair.push(JsValue::from(js_string!(value.as_str())), context)?;
        headers.push(pair, context)?;
    }
    reply.set(js_string!("headers"), headers, false, context)?;
    // So a page can tell an opaque response from a failed one. Both have no
    // body and no headers; only one of them means "the request was made and
    // you may not read the answer".
    reply.set(js_string!("opaque"), outcome.opaque, false, context)?;
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
    let comments = host.comments.borrow();
    for child in &node.children {
        serialise(&doc, &comments, *child, &mut out);
    }
    Ok(js_string!(out).into())
}

/// Elements that never have children or a closing tag.
const VOID_ELEMENTS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
    "source", "track", "wbr",
];

/// Serialise one node and its subtree.
enum Step<'a> {
    /// Serialise this node and push its children.
    Node(usize),
    /// The closing tag an element owes, after its children.
    Close(&'a str),
}

/// Serialise a subtree.
///
/// Iterative, with its own stack. The recursive version was the third door into
/// the deep-tree problem that ends the process rather than the page:
/// `el.innerHTML = "<div>".repeat(20000)` is four characters of JavaScript, and
/// script can read the result back in the same turn it built it, before any
/// layout and so before `crate::engine::prune_deep_nesting` applies. A stack
/// overflow is a `SIGSEGV` with no panic to catch.
fn serialise(
    doc: &blitz_dom::BaseDocument,
    comments: &std::collections::HashMap<usize, String>,
    id: usize,
    out: &mut String,
) {
    let mut stack: Vec<Step<'_>> = vec![Step::Node(id)];
    while let Some(step) = stack.pop() {
        let id = match step {
            Step::Close(name) => {
                out.push_str("</");
                out.push_str(name);
                out.push('>');
                continue;
            }
            Step::Node(id) => id,
        };
        let Some(node) = doc.get_node(id) else { continue };

        // Children are pushed in reverse so they come off in document order.
        let push_children = |stack: &mut Vec<Step<'_>>| {
            for child in node.children.iter().rev() {
                stack.push(Step::Node(*child));
            }
        };

        match &node.data {
            blitz_dom::NodeData::Comment => {
                out.push_str("<!--");
                out.push_str(comments.get(&id).map(String::as_str).unwrap_or(""));
                out.push_str("-->");
            }
            blitz_dom::NodeData::Text(text) => {
                escape_text(&text.content, out);
            }
            blitz_dom::NodeData::Element(el) | blitz_dom::NodeData::AnonymousBlock(el) => {
                let name = el.name.local.as_ref();
                out.push('<');
                out.push_str(name);
                if let Some(attrs) = node.attrs() {
                    for attr in attrs {
                        out.push(' ');
                        out.push_str(attr.name.local.as_ref());
                        out.push_str("=\"");
                        escape_attribute(&attr.value, out);
                        out.push('"');
                    }
                }
                out.push('>');

                if VOID_ELEMENTS.contains(&name) {
                    continue;
                }
                stack.push(Step::Close(name));
                push_children(&mut stack);
            }
            _ => push_children(&mut stack),
        }
    }
}

/// Text content, with the three characters that would otherwise reopen markup.
fn escape_text(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
}

/// An attribute value, which additionally must not close its own quote.
fn escape_attribute(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
}

/// The serialised markup *of* a node, itself included.
fn outer_html(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    if doc.get_node(id).is_none() {
        return Ok(JsValue::null());
    }
    let mut out = String::new();
    serialise(&doc, &host.comments.borrow(), id, &mut out);
    Ok(js_string!(out).into())
}

/// A node's border box in viewport coordinates: `[x, y, width, height]`.
fn scroll_to_node(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    settle_layout(&host);
    let mut doc = host.dom.borrow_mut();

    if doc.get_node(id).is_none() {
        return Ok(JsValue::undefined());
    }

    // Absolute position, by walking to the root. The same sum `rect` makes,
    // before the scroll offset is taken back out of it.
    let mut y = 0.0f32;
    let mut current = Some(id);
    for _ in 0..256 {
        let Some(node_id) = current else { break };
        let Some(node) = doc.get_node(node_id) else { break };
        y += node.final_layout.location.y;
        current = node.parent;
    }

    let viewport_height = doc.viewport().window_size.1 as f64;
    let max = (document_height(&doc) - viewport_height).max(0.0) as f32;

    doc.set_viewport_scroll(blitz_dom::Point {
        x: 0.0,
        y: y.clamp(0.0, max) as f64,
    });
    Ok(JsValue::undefined())
}

/// One entry point for every canvas drawing call.
fn canvas_op(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let op = arg_string(args, 1, context)?;

    // The numeric arguments, in order. A string argument (a colour, a cap) is
    // read separately below, so this stays one shape.
    let mut numbers: Vec<f64> = Vec::new();
    let mut text = String::new();
    if let Some(list) = args.get(2)
        && let Some(object) = list.as_object()
        && let Ok(array) = boa_engine::object::builtins::JsArray::from_object(object.clone())
    {
        let length = array.length(context).unwrap_or(0);
        for at in 0..length {
            let value = array.get(at, context)?;
            if value.is_string() {
                text = value.to_string(context)?.to_std_string_escaped();
            } else {
                numbers.push(value.to_number(context)?);
            }
        }
    }

    let host = host(context)?;
    let mut canvases = host.canvases.borrow_mut();
    let Some(canvas) = canvases.get_mut(id) else {
        // No surface for this node: the caller never asked for a context.
        return Ok(JsValue::from(false));
    };

    let n = |at: usize| -> f64 { numbers.get(at).copied().unwrap_or(0.0) };
    let handled = match op.as_str() {
        "save" => {
            canvas.save();
            true
        }
        "restore" => {
            canvas.restore();
            true
        }
        "fillStyle" => canvas.set_fill_style(&text),
        "strokeStyle" => canvas.set_stroke_style(&text),
        "lineWidth" => {
            canvas.set_line_width(n(0));
            true
        }
        "globalAlpha" => {
            canvas.set_global_alpha(n(0));
            true
        }
        "lineCap" => {
            canvas.set_line_cap(&text);
            true
        }
        "lineJoin" => {
            canvas.set_line_join(&text);
            true
        }
        "translate" => {
            canvas.translate(n(0), n(1));
            true
        }
        "scale" => {
            canvas.scale(n(0), n(1));
            true
        }
        "rotate" => {
            canvas.rotate(n(0));
            true
        }
        "transform" => {
            canvas.transform(n(0), n(1), n(2), n(3), n(4), n(5));
            true
        }
        "setTransform" => {
            canvas.set_transform(n(0), n(1), n(2), n(3), n(4), n(5));
            true
        }
        "resetTransform" => {
            canvas.reset_transform();
            true
        }
        "beginPath" => {
            canvas.begin_path();
            true
        }
        "closePath" => {
            canvas.close_path();
            true
        }
        "moveTo" => {
            canvas.move_to(n(0), n(1));
            true
        }
        "lineTo" => {
            canvas.line_to(n(0), n(1));
            true
        }
        "quadraticCurveTo" => {
            canvas.quad_to(n(0), n(1), n(2), n(3));
            true
        }
        "bezierCurveTo" => {
            canvas.curve_to(n(0), n(1), n(2), n(3), n(4), n(5));
            true
        }
        "rect" => {
            canvas.rect(n(0), n(1), n(2), n(3));
            true
        }
        "arc" => {
            canvas.arc(n(0), n(1), n(2), n(3), n(4), n(5) != 0.0);
            true
        }
        "fill" => {
            if !text.is_empty() {
                canvas.set_fill_rule(&text);
            }
            canvas.fill();
            true
        }
        "stroke" => {
            canvas.stroke();
            true
        }
        "fillRect" => {
            canvas.fill_rect(n(0), n(1), n(2), n(3));
            true
        }
        "strokeRect" => {
            canvas.stroke_rect(n(0), n(1), n(2), n(3));
            true
        }
        "clearRect" => {
            canvas.clear_rect(n(0), n(1), n(2), n(3));
            true
        }
        // Everything else is genuinely not built. Answering `false` is what
        // puts its name in front of the agent instead of leaving a blank
        // canvas to be explained.
        _ => false,
    };

    if handled {
        // A drawn canvas has to reach the page, and the page is laid out from
        // the tree. Marking the document dirty is what gets the surface
        // composited on the next resolve.
        *host.dirty.borrow_mut() = true;
    }
    Ok(JsValue::from(handled))
}

/// Create or resize the surface behind a `<canvas>`, and report its size.
fn canvas_size(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let width = args.get_or_undefined(1).to_number(context)? as u32;
    let height = args.get_or_undefined(2).to_number(context)? as u32;
    let reset = args.get_or_undefined(3).to_boolean();

    let host = host(context)?;
    let mut canvases = host.canvases.borrow_mut();
    // A refusal is an error the page can see, like `getRandomValues` over its
    // own cap. See `Canvases::afford`: the ceiling is on every canvas in the
    // document together, because bounding one at 8192 a side and not bounding
    // how many there are bounds nothing.
    let refuse = |why: String| -> JsResult<JsValue> {
        Err(boa_engine::JsNativeError::error().with_message(why).into())
    };
    let size = {
        let canvas = match canvases.get_or_create(id, width, height) {
            Ok(canvas) => canvas,
            Err(why) => return refuse(why),
        };
        (canvas.width(), canvas.height())
    };
    let size = if reset {
        if let Err(why) = canvases.resize(id, width, height) {
            return refuse(why);
        }
        match canvases.get(id) {
            Some(canvas) => (canvas.width(), canvas.height()),
            None => size,
        }
    } else {
        size
    };

    let array = boa_engine::object::builtins::JsArray::new(context)?;
    array.push(JsValue::from(size.0 as f64), context)?;
    array.push(JsValue::from(size.1 as f64), context)?;
    Ok(array.into())
}

/// The surface as a `data:` URL, for `toDataURL`.
fn canvas_png(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let canvases = host.canvases.borrow();
    let Some(canvas) = canvases.get(id) else {
        return Ok(JsValue::null());
    };
    let Some(png) = canvas.to_png() else {
        return Ok(JsValue::null());
    };
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
    Ok(JsValue::from(js_string!(format!(
        "data:image/png;base64,{encoded}"
    ))))
}

fn rect(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    settle_layout(&host);
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
    let array = boa_engine::object::builtins::JsArray::new(context)?;
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

/// Every computed value Stylo can resolve, which is nearly all of CSS.
fn computed_style(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let property = arg_string(args, 1, context)?.to_lowercase();
    let host = host(context)?;

    // Recompute the cascade first if the tree moved since it last ran.
    settle_layout(&host);

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
        // No primary styles means the node is not rendered: `display: none`
        // is the honest answer for a visibility question about it.
        return Ok(match property.as_str() {
            "display" => js_string!("none").into(),
            _ => js_string!("").into(),
        });
    };

    use style_traits::ToCss as _;
    // `display` used to be answered from `node.display_constructed_as`, which is what the *box
    // tree* built, not what the cascade computed.

    use style::properties::{PropertyDeclarationId, PropertyId};
    let answer = match PropertyId::parse_enabled_for_all_content(&property) {
        // A shorthand resolves to `None` here and so names itself: its computed
        // value is its longhands re-serialised, and getting that subtly wrong is
        // worse than not answering, because a caller comparing two `border`
        // strings would be told two different borders match.
        Ok(PropertyId::NonCustom(id)) => id
            .as_longhand()
            .map(|longhand| styles.computed_value_to_string(PropertyDeclarationId::Longhand(longhand))),
        // `--custom-property`, which is how real pages theme themselves, so this
        // was a live gap rather than an obscure one.
        //
        // Looked up by walking `property_at`, which is the public way in: the keyed
        // `get` wants a `PropertyDescriptors` for the registration, and an
        // unregistered custom property, which is nearly all of them, has none. An
        // unset one answers "" rather than naming itself, because "" is what a
        // browser says and the property is not *missing*, only unset.
        Ok(PropertyId::Custom(name)) => {
            let customs = styles.custom_properties();
            let mut answer = String::new();
            let mut index = 0usize;
            while let Some((key, value)) = customs.property_at(index) {
                if *key == name {
                    answer = value.as_ref().map(|v| v.to_css_string()).unwrap_or_default();
                    break;
                }
                index += 1;
            }
            Some(answer)
        }
        Err(()) => None,
    };

    Ok(match answer {
        Some(value) => js_string!(value).into(),
        None => {
            host.unsupported
                .borrow_mut()
                .record(&format!("getComputedStyle({property})"));
            js_string!("").into()
        }
    })
}

/// The text as rendered: `innerText`, walked natively.
fn inner_text(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    // The walk reads computed style (`display` for what is rendered,
    // `white-space-collapse` for how text folds) so the cascade must be
    // current: a page that sets `div.style.whiteSpace = "pre"` and reads
    // `innerText` on the next line means the pre.
    settle_layout(&host);
    let doc = host.dom.borrow();

    // An unrendered element falls back to `textContent`, per the spec's first
    // step, "not being rendered" is decided before any of the text rules run.
    let rendered = doc
        .get_node(id)
        .and_then(|n| n.primary_styles())
        .map(|s| !s.clone_display().is_none())
        .unwrap_or(false);
    if !rendered {
        let mut out = String::new();
        collect_text_content(&doc, id, &mut out);
        return Ok(js_string!(out).into());
    }

    let mut segments = Vec::new();
    collect_rendered_text(&doc, id, &mut segments);
    Ok(js_string!(assemble_inner_text(segments)).into())
}

/// Iterative, with its own stack, like [`serialise`] above.
fn collect_text_content(doc: &blitz_dom::BaseDocument, id: usize, out: &mut String) {
    let Some(root) = doc.get_node(id) else { return };
    // Reversed on the way in so they come off in document order.
    let mut stack: Vec<usize> = root.children.iter().rev().copied().collect();
    while let Some(next) = stack.pop() {
        let Some(node) = doc.get_node(next) else { continue };
        match &node.data {
            blitz_dom::NodeData::Text(text) => out.push_str(&text.content),
            _ => stack.extend(node.children.iter().rev().copied()),
        }
    }
}

/// One piece of what `innerText` will report, before the joins are decided.
enum TextSegment {
    /// Text with its whitespace already processed per its `white-space`.
    /// `preserved` protects it from the edge-trimming the joins apply.
    Text { content: String, preserved: bool },
    /// A block boundary. Runs of these collapse to one newline.
    BlockBreak,
    /// A `<br>`. Every one of these is a newline the author asked for.
    HardBreak,
}

fn collect_rendered_text(
    doc: &blitz_dom::BaseDocument,
    id: usize,
    out: &mut Vec<TextSegment>,
) {
    use style::properties::longhands::white_space_collapse::computed_value::T as Collapse;
    let Some(node) = doc.get_node(id) else { return };
    // The text children take their whitespace rules from *this* element's
    // computed style, which is what makes `<pre>` and `white-space: pre` on a
    // div behave identically.
    let collapse = node
        .primary_styles()
        .map(|s| s.get_inherited_text().clone_white_space_collapse())
        .unwrap_or(Collapse::Collapse);
    for child in node.children.iter() {
        let Some(kid) = doc.get_node(*child) else { continue };
        match &kid.data {
            blitz_dom::NodeData::Text(text) => {
                let raw = text.content.replace("\r\n", "\n").replace('\r', "\n");
                match collapse {
                    Collapse::Preserve | Collapse::BreakSpaces => {
                        out.push(TextSegment::Text { content: raw, preserved: true });
                    }
                    Collapse::PreserveBreaks => {
                        // `pre-line`: spaces collapse, the newlines stay real.
                        let mut content = String::with_capacity(raw.len());
                        let mut in_spaces = false;
                        for ch in raw.chars() {
                            if ch == ' ' || ch == '\t' {
                                in_spaces = true;
                                continue;
                            }
                            if in_spaces {
                                content.push(' ');
                                in_spaces = false;
                            }
                            content.push(ch);
                        }
                        if in_spaces {
                            content.push(' ');
                        }
                        out.push(TextSegment::Text { content, preserved: true });
                    }
                    Collapse::Collapse => {
                        let mut content = String::with_capacity(raw.len());
                        let mut in_spaces = false;
                        for ch in raw.chars() {
                            if ch.is_ascii_whitespace() {
                                in_spaces = true;
                                continue;
                            }
                            if in_spaces {
                                content.push(' ');
                                in_spaces = false;
                            }
                            content.push(ch);
                        }
                        if in_spaces {
                            content.push(' ');
                        }
                        out.push(TextSegment::Text { content, preserved: false });
                    }
                }
            }
            blitz_dom::NodeData::Element(el) | blitz_dom::NodeData::AnonymousBlock(el) => {
                let name = el.name.local.as_ref();
                if matches!(name, "script" | "style" | "template" | "head" | "title") {
                    continue;
                }
                if name == "br" {
                    out.push(TextSegment::HardBreak);
                    continue;
                }
                // Not rendered, not read. The same rule the snapshot applies,
                // and the reason `innerText` is worth having over `textContent`
                // at all: a hidden menu is not text the user can see.
                let display = match kid.primary_styles() {
                    None => {
                        continue;
                    }
                    Some(styles) => styles.clone_display(),
                };
                if display.is_none() {
                    continue;
                }
                use style_traits::ToCss as _;
                let rendered = display.to_css_string();
                let inline = rendered.starts_with("inline") || rendered == "contents";
                if !inline {
                    out.push(TextSegment::BlockBreak);
                }
                collect_rendered_text(doc, *child, out);
                if !inline {
                    out.push(TextSegment::BlockBreak);
                }
            }
            _ => {}
        }
    }
}

/// Join the segments: block-break runs become one newline, hard breaks keep
/// their count, collapsible text sheds the spaces that touch a break or an
/// edge, and preserved text sheds nothing, which is what `<pre>` means.
fn assemble_inner_text(segments: Vec<TextSegment>) -> String {
    // Each entry is a rendered line-ish chunk: (text, preserved-edges flags
    // folded in already). Build the output by walking segments and tracking
    // whether a break is pending and how many hard breaks it contains.
    let mut result = String::new();
    let mut pending_block = false;
    let mut pending_hard = 0usize;
    let mut tail_preserved = false;
    for segment in segments {
        match segment {
            TextSegment::BlockBreak => pending_block = true,
            TextSegment::HardBreak => pending_hard += 1,
            TextSegment::Text { content, preserved } => {
                let mut piece = content;
                if piece.is_empty() {
                    continue;
                }
                if !preserved && (piece.trim() == "") && (pending_block || pending_hard > 0 || result.is_empty()) {
                    // Whitespace-only filler beside a break or at the start
                    // never renders.
                    continue;
                }
                let breaks = if pending_hard > 0 {
                    pending_hard
                } else if pending_block && !result.is_empty() {
                    1
                } else {
                    0
                };
                if breaks > 0 {
                    if !preserved {
                        // A collapsible space never survives against a break.
                        piece = piece.trim_start_matches(' ').to_string();
                    }
                    if !tail_preserved {
                        while result.ends_with(' ') {
                            result.pop();
                        }
                    }
                    result.extend(std::iter::repeat_n('\n', breaks));
                } else if result.is_empty() && !preserved {
                    piece = piece.trim_start_matches(' ').to_string();
                }
                if !piece.is_empty() {
                    result.push_str(&piece);
                    tail_preserved = preserved;
                }
                pending_block = false;
                pending_hard = 0;
            }
        }
    }
    // A trailing collapsible space vanishes; a preserved one, the end of a
    // `<pre>abc </pre>`, is content and stays.
    if !tail_preserved {
        while result.ends_with(' ') {
            result.pop();
        }
    }
    result
}

/// The canonical name for an encoding label, or null if it is not one.
fn encoding_for(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let label = arg_string(args, 0, context)?;
    Ok(match encoding_rs::Encoding::for_label(label.trim().as_bytes()) {
        Some(encoding) => js_string!(encoding.name().to_ascii_lowercase()).into(),
        None => JsValue::null(),
    })
}

/// Decode bytes as the named encoding.
///
/// `fatal` is the decoder's own option: a page that asked for fatal decoding
/// wants an error rather than replacement characters, and answering with U+FFFD
/// either way would hide malformed input from the caller who explicitly asked
/// to be told about it.
fn decode_bytes(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let label = arg_string(args, 0, context)?;
    let fatal = args.get_or_undefined(2).to_boolean();
    let Some(encoding) = encoding_rs::Encoding::for_label(label.trim().as_bytes()) else {
        return Err(boa_engine::JsNativeError::range()
            .with_message(format!("{label} is not a known encoding"))
            .into());
    };

    let array = boa_engine::object::builtins::JsArray::from_object(
        args.get_or_undefined(1).as_object().ok_or_else(|| {
            boa_engine::JsNativeError::typ().with_message("decode expects an array of bytes")
        })?,
    )?;
    let length = array.length(context)? as usize;
    let mut bytes = Vec::with_capacity(length);
    for index in 0..length {
        bytes.push(array.get(index as u64, context)?.to_number(context)? as u8);
    }

    if fatal {
        match encoding.decode_without_bom_handling_and_without_replacement(&bytes) {
            Some(text) => Ok(js_string!(text.as_ref()).into()),
            None => Err(boa_engine::JsNativeError::typ()
                .with_message("the bytes are not valid in this encoding")
                .into()),
        }
    } else {
        let (text, _) = encoding.decode_without_bom_handling(&bytes);
        Ok(js_string!(text.as_ref()).into())
    }
}

/// Re-encode the query part of a URL *as written*, in the document's encoding.
///
/// Operates on the text a page handed over, before any parser has touched it,
/// so an escape the author wrote is left exactly as they wrote it and only
/// literal characters are converted. The query runs from the first `?` to the
/// `#` that ends it, or to the end.
fn rewrite_query(href: &str, encoding: &'static encoding_rs::Encoding) -> String {
    let Some(start) = href.find('?') else {
        return href.to_string();
    };
    let rest = &href[start + 1..];
    let end = rest.find('#').unwrap_or(rest.len());
    let query = &rest[..end];
    if query.is_ascii() {
        // Nothing here needs a decision, and this is the common case even in a
        // legacy document.
        return href.to_string();
    }
    format!(
        "{}?{}{}",
        &href[..start],
        crate::encoding::encode_query(query, encoding),
        &rest[end..]
    )
}

/// What the document is written in, as the canonical label.
fn document_encoding(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    let name = host.encoding.borrow().name().to_string();
    Ok(js_string!(name).into())
}

/// Run a tree mutation, and report rather than abort if the layer beneath panics.
fn guard_mutation(host: &HostHandle, what: &str, body: impl FnOnce()) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    if let Err(payload) = outcome {
        let detail = payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "the layout engine panicked".to_string());
        let first = detail.lines().next().unwrap_or("").to_string();
        super::host::push_console(
            &mut host.console.borrow_mut(),
            ConsoleLine::engine("error", format!("{what} was refused by the layout engine: {first}")),
        );
        host.unsupported
            .borrow_mut()
            .record(&format!("{what} on content the layout engine cannot resolve"));
    }
}

/// Bring style and layout up to date, if script has changed the tree since they last ran.
fn settle_layout(host: &HostHandle) {
    if *host.styles_stale.borrow() {
        *host.styles_stale.borrow_mut() = false;
        // Through the engine's helper, not `resolve` directly. This was the fourth
        // door into the deep-tree problem: layout recurses, and `getComputedStyle`
        // on a tree script has just built reaches it before the settle loop does, so
        // `el.innerHTML = "<div>".repeat(20000)` then one style read leaves the
        // process gone with no panic to catch.
        //
        // It also picks up `guard_layout`, which matters more here than anywhere: a
        // panic raised inside a native binding unwinds through the JavaScript
        // engine, across a `Gc` that does not expect it.
        let _ = crate::engine::lay_out(&host.dom);
    }
}

/// Whether a selector parses, so the prelude can throw where a browser throws.
///
/// `querySelector("!!!")` is a `SyntaxError` in every browser, and this engine
/// answered `null`: indistinguishable from "there is no such element". A page
/// with a typo in a selector was told its element does not exist, which is the
/// plausible wrong answer this engine keeps removing: it sends the caller down
/// the not-found branch instead of showing them their mistake.
fn valid_selector(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let selector = arg_string(args, 0, context)?;
    let host = host(context)?;
    let doc = host.dom.borrow();
    let valid = doc.try_parse_selector_list(&selector).is_ok();
    Ok(JsValue::from(valid))
}

/// Whether this is a CSS property at all, which is what `in` on a computed
/// style is asking.
///
/// `"color" in getComputedStyle(el)` was *false* for every property, because the
/// computed-style object is a proxy with only a `get` trap and `in` uses `has`.
/// WPT's `test_computed_value` asserts exactly that on its first line, and it is
/// the standard helper for CSS parsing tests, so thousands of subtests failed
/// before they ever compared a value, including all of `css-color`, where Stylo
/// supported every feature under test.
fn is_css_property(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = arg_string(args, 0, context)?;
    let known = style::properties::PropertyId::parse_enabled_for_all_content(&name).is_ok();
    Ok(JsValue::from(known))
}

/// Whether Stylo can parse this declaration, which is what `CSS.supports` asks.
///
/// Answered by actually parsing it rather than by consulting a list. A list
/// would be a second opinion about what this engine supports, and the two would
/// drift the moment Stylo moved, and `CSS.supports` is a question pages ask
/// precisely so they can take a *different code path*, so a wrong answer here
/// does not degrade, it misroutes.
fn supports_css(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let property = arg_string(args, 0, context)?;
    let value = arg_string(args, 1, context)?;

    use style::properties::{PropertyDeclaration, PropertyId, SourcePropertyDeclaration};
    use style::stylesheets::{CssRuleType, Origin, UrlExtraData};

    let Ok(base) = ::url::Url::parse("about:blank") else {
        return Ok(JsValue::from(false));
    };
    let url_data = UrlExtraData(style::servo_arc::Arc::new(base));
    let parser_context = style::parser::ParserContext::new(
        Origin::Author,
        &url_data,
        Some(CssRuleType::Style),
        style_traits::ParsingMode::DEFAULT,
        style::context::QuirksMode::NoQuirks,
        Default::default(),
        None,
        None,
        Default::default(),
    );

    // `parse_enabled_for_all_content`, not plain `parse`: the plain form
    // accepts pref-gated and internal properties (`-moz-*`), and answering
    // "supported" for a property no page rule could ever use is exactly the
    // misrouting `CSS.supports` exists to avoid. WPT cross-checks this answer
    // against the style declaration's own surface, which uses the same gate.
    let Ok(property_id) = PropertyId::parse_enabled_for_all_content(&property) else {
        return Ok(JsValue::from(false));
    };
    let _ = &parser_context;
    let mut declaration = SourcePropertyDeclaration::default();
    let mut input = cssparser::ParserInput::new(&value);
    let mut parser = cssparser::Parser::new(&mut input);
    let supported = PropertyDeclaration::parse_into(
        &mut declaration,
        property_id,
        &parser_context,
        &mut parser,
    )
    .is_ok();
    Ok(JsValue::from(supported))
}

/// One declaration, parsed and serialised back: the CSSOM "specified value".
fn serialize_css_value(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let property = arg_string(args, 0, context)?;
    let value = arg_string(args, 1, context)?;

    use style::properties::{
        Importance, PropertyDeclaration, PropertyDeclarationBlock, PropertyId,
        SourcePropertyDeclaration,
    };
    use style::stylesheets::{CssRuleType, Origin, UrlExtraData};

    // Property first: it is the cheap rejection, and it saves building a parser
    // context for a name no rule could use.
    let Ok(property_id) = PropertyId::parse_enabled_for_all_content(&property) else {
        return Ok(js_string!("").into());
    };

    // The base URL is parsed once per thread, not once per call. Unlike
    // `CSS.supports` above, which a page asks a handful of times, this runs
    // on every inline-style property read, so re-parsing `about:blank` and
    // rebuilding it each time would put a URL parse on a path frameworks touch
    // constantly.
    thread_local! {
        static BASE: Option<UrlExtraData> = ::url::Url::parse("about:blank")
            .ok()
            .map(|base| UrlExtraData(style::servo_arc::Arc::new(base)));
    }
    let Some(url_data) = BASE.with(|base| base.clone()) else {
        return Ok(js_string!("").into());
    };
    let parser_context = style::parser::ParserContext::new(
        Origin::Author,
        &url_data,
        Some(CssRuleType::Style),
        style_traits::ParsingMode::DEFAULT,
        style::context::QuirksMode::NoQuirks,
        Default::default(),
        None,
        None,
        Default::default(),
    );
    let mut source = SourcePropertyDeclaration::default();
    let mut input = cssparser::ParserInput::new(&value);
    let mut parser = cssparser::Parser::new(&mut input);
    if PropertyDeclaration::parse_into(&mut source, property_id.clone(), &parser_context, &mut parser)
        .is_err()
    {
        return Ok(js_string!("").into());
    }

    // Through a block rather than serialising the declarations one by one: a
    // shorthand parses into its longhands, and only the block knows how to put
    // them back together as the shorthand the page asked for.
    let mut block = PropertyDeclarationBlock::new();
    for declaration in source.declarations.drain(..) {
        block.push(declaration, Importance::Normal);
    }
    let mut out = String::new();
    if block.property_value_to_css(&property_id, &mut out).is_err() {
        return Ok(js_string!("").into());
    }
    Ok(js_string!(out).into())
}

/// Parse a URL against an optional base, using the same parser the broker uses.
fn url_with_userinfo(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let href = arg_string(args, 0, context)?;
    let field = arg_string(args, 1, context)?;
    let value = arg_string(args, 2, context)?;
    let Ok(mut url) = url::Url::parse(&href) else {
        return Ok(JsValue::null());
    };
    let wrote = if field == "username" {
        url.set_username(&value)
    } else {
        // An empty password is *absent*, not present-and-empty: the
        // serialisation drops the colon, which is what a browser shows.
        url.set_password(if value.is_empty() { None } else { Some(&value) })
    };
    if wrote.is_err() {
        return Ok(JsValue::null());
    }
    Ok(js_string!(url.to_string()).into())
}

fn parse_url(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let href = arg_string(args, 0, context)?;
    let base = arg_string(args, 1, context).unwrap_or_default();
    let encoding = host(context)
        .map(|host| *host.encoding.borrow())
        .unwrap_or(encoding_rs::UTF_8);

    // A query belongs to the document, not to UTF-8, and it has to be encoded *before* parsing
    // rather than after.
    let href = if encoding == encoding_rs::UTF_8 {
        href
    } else {
        rewrite_query(&href, encoding)
    };

    let parsed = if base.is_empty() {
        url::Url::parse(&href)
    } else {
        url::Url::parse(&base).and_then(|base| base.join(&href))
    };

    let Ok(url) = parsed else {
        return Ok(JsValue::null());
    };

    let out = boa_engine::object::ObjectInitializer::new(context).build();
    let fields: [(&str, String); 10] = [
        ("href", url.to_string()),
        // The userinfo half. It was read as "" and written nowhere, on the
        // grounds that the parser did not surface it, but `url::Url` has had
        // both all along, so `url-setters-stripping` failed 226 subtests
        // against a component this engine could already see.
        ("username", url.username().to_string()),
        ("password", url.password().unwrap_or_default().to_string()),
        ("protocol", format!("{}:", url.scheme())),
        ("host", url.host_str().map(|h| match url.port() {
            Some(port) => format!("{h}:{port}"),
            None => h.to_string(),
        }).unwrap_or_default()),
        ("hostname", url.host_str().unwrap_or_default().to_string()),
        ("port", url.port().map(|p| p.to_string()).unwrap_or_default()),
        ("pathname", url.path().to_string()),
        // Empty is not the same as absent to a URL, but it is to `search`:
        // `https://e.com/?` reports "" in a browser, not "?".
        ("search", match url.query() {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        }),
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

/// The viewport a media query is asked about.
///
/// Real numbers, because they are real: the viewport has a fixed size and a
/// known colour scheme, so `(min-width: 900px)` has a correct answer and
/// returning `false` to everything would send responsive layouts down the wrong
/// branch and keep them there.
fn viewport(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    let doc = host.dom.borrow();
    let size = doc.viewport().window_size;

    let out = boa_engine::object::ObjectInitializer::new(context).build();
    out.set(js_string!("width"), size.0 as f64, false, context)?;
    out.set(js_string!("height"), size.1 as f64, false, context)?;
    // The engine renders with `ColorScheme::Light`; saying so is what lets a
    // page pick the palette it will actually be screenshotted in.
    out.set(js_string!("colorScheme"), js_string!("light"), false, context)?;
    Ok(out.into())
}

/// `document.cookie`: the non-`HttpOnly` cookies for this document.
///
/// Deliberately not the wire header. A session credential is almost always
/// `HttpOnly`, and withholding it is what keeps the property that an agent can
/// be logged in without being able to read the thing that makes it so, because
/// anything script can read, script can write into the DOM, and the agent reads
/// the DOM.
fn read_cookies(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    Ok(js_string!(host.broker.document_cookie(&host.base)).into())
}

/// `document.cookie = "..."`: store one cookie as the current document.
fn write_cookie(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let header = arg_string(args, 0, context)?;
    let host = host(context)?;
    // Not `store`: see `Jar::store_from_script`. Script may not overwrite an
    // `HttpOnly` cookie, nor set one.
    let stored = host.broker.store_cookie(&host.base, &header);
    Ok(JsValue::from(stored as f64))
}


// ── websockets ───────────────────────────────────────────────────────────────
//
// The realm is single-threaded and `!Send`, so a socket cannot be read from it.
// These follow the same shape the fetch path already uses: a worker thread does
// the blocking read and hands frames back over a channel, and the page collects
// them at a point the engine chooses, here once per settle round.
//
// That choice has a consequence worth stating rather than leaving to be
// discovered. A message that arrives while nothing is running is delivered at
// the next verb, not the moment it lands. The session is idle by design, which
// is what makes it cost nothing at rest, so there is no thread of control to
// deliver into. For an agent driving a page this is invisible; for anything
// expecting real-time delivery it is not.

/// One drained event as `[kind, payload, name]`.
///
/// Always three elements, and `name` carries the event type for a server-sent
/// event. It used to be packed into the payload as a first line and unpacked by
/// a heuristic on the JS side, which read a plain multi-line
/// `data: one\ndata: two` as an event *named* `one`.
fn entry_for(
    event: crate::wsclient::Event,
    context: &mut Context,
) -> JsResult<boa_engine::object::builtins::JsArray> {
    let (kind, payload, name) = match event {
        crate::wsclient::Event::Open => ("open", String::new(), String::new()),
        crate::wsclient::Event::Message(text) => ("message", text, String::new()),
        crate::wsclient::Event::Named { name, data } => ("message", data, name),
        crate::wsclient::Event::Closed(why) => ("close", why, String::new()),
        crate::wsclient::Event::Failed(why) => ("error", why, String::new()),
    };
    let entry = boa_engine::object::builtins::JsArray::new(context)?;
    entry.push(js_string!(kind), context)?;
    entry.push(js_string!(payload.as_str()), context)?;
    entry.push(js_string!(name.as_str()), context)?;
    Ok(entry)
}

/// Open a socket. Returns its id, or throws with the reason.
fn socket_open(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = arg_string(args, 0, context)?;
    let host = host(context)?;
    let url = host.base.join(&raw).map_err(|e| {
        boa_engine::JsNativeError::syntax().with_message(format!("`{raw}` is not a URL: {e}"))
    })?;

    if let Err(why) = room_for_a_channel(&host) {
        return Err(boa_engine::JsNativeError::error().with_message(why).into());
    }

    match host.broker.open_socket(&url, Some(&host.base)) {
        Ok(socket) => {
            let id = host.next_socket.get();
            host.next_socket.set(id + 1);
            host.sockets.borrow_mut().insert(id, socket);
            Ok(JsValue::from(id as f64))
        }
        // A refusal is an answer the page can see, the same as a refused fetch.
        Err(error) => Err(boa_engine::JsNativeError::error().with_message(error).into()),
    }
}

fn socket_send(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)? as u64;
    let text = arg_string(args, 1, context)?;
    let host = host(context)?;
    let socket = host.sockets.borrow().get(&id).cloned();
    match socket {
        None => Ok(JsValue::from(false)),
        Some(socket) => match socket.send(&text) {
            Ok(()) => Ok(JsValue::from(true)),
            Err(error) => Err(boa_engine::JsNativeError::error().with_message(error).into()),
        },
    }
}

fn socket_close(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)? as u64;
    let host = host(context)?;
    if let Some(socket) = host.sockets.borrow_mut().remove(&id) {
        socket.close();
    }
    Ok(JsValue::undefined())
}

/// Everything that has arrived on one socket since the last drain.
///
/// `[[kind, payload], ...]`: `kind` is `open`, `message`, `close` or `error`.
/// Flat pairs rather than objects because the prelude turns them into real
/// events anyway, and building objects here would mean building them twice.
fn socket_drain(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)? as u64;
    let host = host(context)?;
    let socket = host.sockets.borrow().get(&id).cloned();
    let out = boa_engine::object::builtins::JsArray::new(context)?;
    let Some(socket) = socket else {
        return Ok(out.into());
    };

    for event in socket.drain() {
        out.push(entry_for(event, context)?, context)?;
    }

    Ok(out.into())
}

/// Whether this page may hold another long-lived connection.
///
/// See [`crate::script::host::MAX_OPEN_CHANNELS`]: each of these is a thread,
/// and the thread is the resource the session's sandbox profile actually caps.
fn room_for_a_channel(host: &HostHandle) -> Result<(), String> {
    let held = host.sockets.borrow().len() + host.streams.borrow().len();
    channel_room(held)
}

/// The arithmetic on its own, so the bound can be tested without a socket.
pub(crate) fn channel_room(held: usize) -> Result<(), String> {
    if held >= crate::script::host::MAX_OPEN_CHANNELS {
        return Err(format!(
            "this page already holds {held} open connections, which is the most one page may \
             have at once. Close one before opening another; the per-navigation request \
             budget is the separate bound on how many it may open in total."
        ));
    }
    Ok(())
}

// ── server-sent events ───────────────────────────────────────────────────────
//
// The same shape as the socket primitives above, and deliberately so: two
// long-lived connections with one delivery mechanism between them, rather than
// two mechanisms that drift.

fn sse_open(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = arg_string(args, 0, context)?;
    let host = host(context)?;
    let url = host.base.join(&raw).map_err(|e| {
        boa_engine::JsNativeError::syntax().with_message(format!("`{raw}` is not a URL: {e}"))
    })?;

    if let Err(why) = room_for_a_channel(&host) {
        return Err(boa_engine::JsNativeError::error().with_message(why).into());
    }

    match host.broker.open_event_stream(&url, Some(&host.base)) {
        Ok(stream) => {
            let id = host.next_socket.get();
            host.next_socket.set(id + 1);
            host.streams.borrow_mut().insert(id, stream);
            Ok(JsValue::from(id as f64))
        }
        Err(error) => Err(boa_engine::JsNativeError::error().with_message(error).into()),
    }
}

fn sse_close(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)? as u64;
    let host = host(context)?;
    if let Some(stream) = host.streams.borrow_mut().remove(&id) {
        stream.close();
    }
    Ok(JsValue::undefined())
}

fn sse_drain(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)? as u64;
    let host = host(context)?;
    let stream = host.streams.borrow().get(&id).cloned();
    let out = boa_engine::object::builtins::JsArray::new(context)?;
    let Some(stream) = stream else {
        return Ok(out.into());
    };

    for event in stream.drain() {
        out.push(entry_for(event, context)?, context)?;
    }

    Ok(out.into())
}
