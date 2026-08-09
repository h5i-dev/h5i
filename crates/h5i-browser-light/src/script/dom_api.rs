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
        ("fetchStart", 3, fetch_start),
        ("fetchDrain", 0, fetch_drain),
        ("fetchPending", 0, fetch_pending),
        ("userAgent", 0, user_agent),
        ("innerHtml", 1, inner_html),
        ("outerHtml", 1, outer_html),
        ("rect", 1, rect),
        ("computedStyle", 2, computed_style),
        ("parseUrl", 2, parse_url),
        ("viewport", 0, viewport),
        ("readCookies", 0, read_cookies),
        ("writeCookie", 1, write_cookie),
        ("scrollToNode", 1, scroll_to_node),
        ("createComment", 1, create_comment),
        ("scrollMetrics", 1, scroll_metrics),
        ("setScrollTop", 2, set_scroll_top),
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

/// A real comment node, because a marker that is secretly a text node shows up
/// in `textContent` and in the outline an agent reads.
///
/// Template libraries anchor themselves to comments — an empty list leaves
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
/// always asks for the next in the same expression — `el.scrollTop + el.clientHeight
/// >= el.scrollHeight` is how every "am I at the bottom" check is written.
fn scroll_metrics(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
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

    let array = boa_engine::object::builtins::JsArray::new(context);
    for value in values {
        array.push(JsValue::from(value), context)?;
    }
    Ok(array.into())
}

/// `documentElement` and `body` both stand for the page — pages read scroll
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
/// grew past the viewport — the same trap `Page::max_scroll_y` documents, and
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
/// The one user agent, so the prelude cannot hold a second copy that drifts.
fn user_agent(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(js_string!(crate::net::USER_AGENT)))
}

/// Accept a request from script and hand back a ticket.
///
/// Nothing goes on the wire here. The request joins an ordered queue, and
/// [`fetch_drain`] starts it when a slot is free — which is what makes two
/// `fetch` calls actually overlap instead of running one after the other. The
/// old binding did the whole round trip inline, so a page that fanned out ten
/// requests paid for them in series and every SPA waterfall was our own.
fn fetch_start(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg_string(args, 0, context)?;
    let method = arg_string(args, 1, context).unwrap_or_else(|_| "GET".to_string());
    let body = arg_string(args, 2, context).unwrap_or_default();
    let host = host(context)?;

    let resolved = match host.base.join(&target) {
        Ok(url) => url,
        Err(error) => return reply_error(&format!("`{target}` is not a URL: {error}"), context),
    };
    let id = host.next_fetch.get();
    host.next_fetch.set(id + 1);
    host.requests
        .borrow_mut()
        .push(crate::script::host::RequestLink {
            ticket: id,
            url: resolved.to_string(),
            seq: None,
        });
    host.pending_fetches.borrow_mut().insert(
        id,
        crate::script::host::FetchSlot::Queued {
            url: resolved,
            method,
            content_type: (!body.is_empty())
                .then(|| "application/x-www-form-urlencoded".to_string()),
            body: body.into_bytes(),
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
                    let outcome = broker.send_from(
                        &url,
                        crate::receipt::Initiator::Subresource,
                        &method,
                        &body,
                        content_type.as_deref(),
                        Some(&document),
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
        // Exactly one `try_recv` per slot. Asking twice — once to find out
        // whether an answer was there and again to take it — takes the value on
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

    let out = boa_engine::object::builtins::JsArray::new(context);
    for (id, outcome) in arrived {
        let pair = boa_engine::object::builtins::JsArray::new(context);
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
/// Put a node at the top of the viewport, clamped to the document.
///
/// Backs `Element.scrollIntoView`. It moves what a screenshot or a live viewer
/// shows and nothing about what the text outline contains, because that outline
/// covers the whole document regardless of where the viewport sits — so a page
/// that scrolls to its content is not thereby made more readable, only more
/// watchable.
fn scroll_to_node(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg_id(args, 0, context)?;
    let host = host(context)?;
    let mut doc = host.dom.borrow_mut();

    if doc.get_node(id).is_none() {
        return Ok(JsValue::undefined());
    }

    // Absolute position, by walking to the root — the same sum `rect` makes,
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
/// be logged in without being able to read the thing that makes it so — because
/// anything script can read, script can write into the DOM, and the agent reads
/// the DOM.
fn read_cookies(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    Ok(js_string!(host.broker.jar().document_cookie(&host.base)).into())
}

/// `document.cookie = "..."`: store one cookie as the current document.
fn write_cookie(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let header = arg_string(args, 0, context)?;
    let host = host(context)?;
    let stored = host.broker.jar().store(&host.base, [header.as_str()]);
    Ok(JsValue::from(stored as f64))
}
