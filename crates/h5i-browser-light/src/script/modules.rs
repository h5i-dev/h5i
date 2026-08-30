//! ES modules, loaded through the broker like everything else.
//!
//! # Bare specifiers are an error, not a CDN
//!
//! `import "lodash"` has no meaning on the web without an import map: the
//! specifier names nothing a browser can fetch. A loader that quietly rewrites
//! it — Thalora maps bare specifiers to `https://esm.sh/{}` — turns one line of
//! page script into an unrequested request to a third party, chosen by the
//! engine rather than by the page. Inside a sandbox whose entire claim is that
//! every request is policy-checked and receipted, an engine that invents
//! destinations is the wrong kind of helpful.
//!
//! So a bare specifier is refused, by name, with what would have to exist for it
//! to work. The agent reads that and knows the page needs a bundle it did not
//! get; it does not silently acquire a dependency on a CDN.
//!
//! # Every module fetch is a brokered fetch
//!
//! Same broker, same policy, same receipts, and the same document origin, so a
//! module cannot reach the box's dev server from a page the web served
//! (roadmap-history.md §B3.1). A private HTTP client here would be the one
//! request class in the engine with no record.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use boa_engine::module::{ModuleLoader, ModuleRequest, Referrer};
use boa_engine::{Context, JsError, JsNativeError, JsResult, Module, Source};
use url::Url;

use super::host::Host;
use crate::receipt::Initiator;

/// Resolves and fetches modules for one realm.
pub struct BrokerModuleLoader {
    host: Rc<Host>,
    /// Resolved URL to module.
    ///
    /// The trait requires that loading the same `(referrer, specifier)` twice
    /// produce the same result, and a page importing one module from three
    /// places should fetch it once. Keyed by the *resolved* URL rather than the
    /// specifier, because `./a.js` from two directories is two modules and
    /// `../a.js` and `./a.js` can be one.
    cache: RefCell<HashMap<String, Module>>,
    /// Resolved URL to the URL it was actually served from, for
    /// `import.meta.url`. They differ when a redirect moved the module.
    paths: RefCell<HashMap<String, String>>,
}

impl BrokerModuleLoader {
    pub fn new(host: Rc<Host>) -> Self {
        Self {
            host,
            cache: RefCell::new(HashMap::new()),
            paths: RefCell::new(HashMap::new()),
        }
    }

    /// The document URL, used when a referrer has no path of its own — an
    /// inline `<script type="module">`, whose relative imports resolve against
    /// the page that contains it.
    fn base_for(&self, referrer: &Referrer) -> Url {
        referrer
            .path()
            .and_then(|path| Url::parse(&path.to_string_lossy()).ok())
            .unwrap_or_else(|| self.host.base.clone())
    }
}

/// Turn a specifier into a URL, or say why it cannot become one.
///
/// The three cases a browser recognises: absolute URL, root-relative, and
/// path-relative. Everything else is a bare specifier.
pub fn resolve(specifier: &str, base: &Url) -> Result<Url, String> {
    let trimmed = specifier.trim();
    if trimmed.is_empty() {
        return Err("an empty import specifier names nothing".to_string());
    }

    if trimmed.starts_with("./") || trimmed.starts_with("../") || trimmed.starts_with('/') {
        return base
            .join(trimmed)
            .map_err(|error| format!("`{trimmed}` is not a resolvable path: {error}"));
    }

    if let Ok(absolute) = Url::parse(trimmed) {
        if matches!(absolute.scheme(), "http" | "https") {
            return Ok(absolute);
        }
        return Err(format!(
            "`{trimmed}` uses the `{}` scheme, which this engine does not fetch",
            absolute.scheme()
        ));
    }

    Err(format!(
        "bare specifier `{trimmed}` cannot be resolved: this page declares no import map \
         entry for it, and this engine will not invent a CDN. Declare it in a \
         `<script type=\"importmap\">` (the page chooses the destination, and the fetch is \
         then policy-checked and receipted like any other), serve the module at a path the \
         page can name (`./{trimmed}.js`), or ship a bundle with its imports resolved."
    ))
}

impl BrokerModuleLoader {
    /// Where a module was served from, for `import.meta.url`.
    fn url_of(&self, module: &Module) -> Option<String> {
        let path = module.path()?.to_string_lossy().into_owned();
        // The path a module was parsed under *is* its final URL — see the
        // comment where the source is built — so the map is a lookup by either.
        self.paths
            .borrow()
            .values()
            .find(|url| **url == path)
            .cloned()
            .or(Some(path))
    }
}

impl ModuleLoader for BrokerModuleLoader {
    /// Fetch and parse one imported module.
    ///
    /// Async in Boa 0.21, and *synchronous underneath*: the broker blocks, and
    /// this returns an already-finished future. That is deliberate rather than
    /// lazy — a module graph has to be resolved before the page can run at all,
    /// so there is no work to overlap it with, and the borrow of the context is
    /// never held across a suspension point because there is none.
    async fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> JsResult<Module> {
        // Import attributes (`with { type: "json" }`) are deliberately ignored:
        // this engine serves one kind of module, and a page asking for another
        // gets the same answer it would if the file were not there.
        let specifier = request.specifier().to_std_string_escaped();
        let base = self.base_for(&referrer);

        // The page's own mapping, first. Nothing here chooses a destination:
        // an answer means the document wrote one down, and it goes through the
        // same broker and the same policy as any other subresource.
        let mapped = self
            .host
            .import_map
            .borrow()
            .as_ref()
            .and_then(|map| map.resolve(specifier.trim(), &base));

        let resolved = match mapped.map(Ok).unwrap_or_else(|| resolve(&specifier, &base)) {
            Ok(url) => url,
            Err(reason) => {
                // Recorded as well as thrown: a page that fails on a bare
                // specifier should show up in the snapshot's unsupported list,
                // where an agent will see it, rather than only in an exception
                // it may swallow.
                self.host
                    .unsupported
                    .borrow_mut()
                    .record(&format!("import {specifier}"));
                return Err(JsError::from_native(
                    JsNativeError::typ().with_message(reason),
                ));
            }
        };

        if let Some(cached) = self.cache.borrow().get(resolved.as_str()) {
            return Ok(cached.clone());
        }

        let outcome = self.host.broker.send_from(
            &resolved,
            Initiator::Subresource,
            "GET",
            &[],
            None,
            Some(&self.host.base),
        );

        // A module import is a request the page made like any other. It is
        // fetched inline rather than through the ticket queue, so its receipt
        // number is known by the time it is recorded.
        self.host
            .requests
            .borrow_mut()
            .push(crate::script::host::RequestLink {
                ticket: 0,
                url: resolved.to_string(),
                seq: outcome.seq,
            });

        if let Some(error) = outcome.error {
            return Err(JsError::from_native(JsNativeError::typ().with_message(
                format!("could not load module {resolved}: {error}"),
            )));
        }

        // An HTTP error is not a module. The fetch *succeeded* — the server
        // answered — so `outcome.error` is empty, but a 404 body is an error
        // page or nothing at all. Parsing it as JavaScript either throws a
        // syntax error blaming the page's own code, or, for an empty body,
        // succeeds as an empty module and the import silently exports nothing.
        // Both are worse than saying the file is missing.
        let status = outcome.status.unwrap_or(0);
        if !(200..300).contains(&status) {
            return Err(JsError::from_native(JsNativeError::typ().with_message(
                format!("could not load module {resolved}: the server answered {status}"),
            )));
        }

        let body = String::from_utf8_lossy(&outcome.body).into_owned();
        // The resolved URL travels as the module's path so that *its* relative
        // imports resolve against where it was served from, not against the
        // document. Without this, `./b.js` inside `/vendor/a.js` would look for
        // `/b.js`. The *final* URL, so a module moved by a redirect resolves
        // against where it actually came from.
        let final_url = outcome.final_url.to_string();
        let source = Source::from_reader(body.as_bytes(), Some(Path::new(&final_url)));

        // Remember where this module came from, so `import.meta.url` can answer.
        // Without it, `new URL("./x.css", import.meta.url)` gets `undefined` as
        // its base and throws `Invalid URL` — which is how one asset path took
        // down a whole bundle.
        self.paths
            .borrow_mut()
            .insert(resolved.to_string(), final_url.clone());

        let parsed = Module::parse(source, None, &mut context.borrow_mut());
        match parsed {
            Ok(module) => {
                self.cache
                    .borrow_mut()
                    .insert(resolved.to_string(), module.clone());
                Ok(module)
            }
            Err(error) => Err(JsError::from_native(JsNativeError::syntax().with_message(
                format!("{resolved} is not a valid module: {error}"),
            ))),
        }
    }

    /// Fill in `import.meta` for one module.
    ///
    /// `url` only, which is the property that carries weight: bundlers resolve
    /// every sibling asset against it, so a module without one cannot find its
    /// own stylesheet.
    fn init_import_meta(
        self: Rc<Self>,
        import_meta: &boa_engine::JsObject,
        module: &Module,
        context: &mut Context,
    ) {
        if let Some(url) = self.url_of(module) {
            let _ = import_meta.set(
                boa_engine::js_string!("url"),
                boa_engine::js_string!(url.as_str()),
                false,
                context,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://app.example/page/index.html").unwrap()
    }

    #[test]
    fn relative_specifiers_resolve_against_the_importer() {
        assert_eq!(
            resolve("./a.js", &base()).unwrap().as_str(),
            "https://app.example/page/a.js"
        );
        assert_eq!(
            resolve("../lib/b.js", &base()).unwrap().as_str(),
            "https://app.example/lib/b.js"
        );
        assert_eq!(
            resolve("/c.js", &base()).unwrap().as_str(),
            "https://app.example/c.js"
        );
    }

    #[test]
    fn absolute_http_specifiers_are_taken_as_written() {
        assert_eq!(
            resolve("https://cdn.example/d.js", &base()).unwrap().as_str(),
            "https://cdn.example/d.js"
        );
    }

    #[test]
    fn a_bare_specifier_is_refused_and_never_rewritten_to_a_cdn() {
        // The trap this loader exists to avoid. `import "lodash"` must not
        // become a request to a third party the page never named.
        let error = resolve("lodash", &base()).expect_err("refused");
        assert!(error.contains("lodash"), "{error}");
        assert!(error.contains("no import map"), "{error}");
        assert!(
            !error.contains("esm.sh") && !error.contains("cdn"),
            "the message must not point at a CDN either: {error}"
        );
        assert!(error.contains("bundle"), "it says what would work instead: {error}");

        // Scoped packages are bare too, and fail the same way.
        assert!(resolve("@scope/pkg", &base()).is_err());
    }

    #[test]
    fn schemes_this_engine_does_not_fetch_are_named() {
        let error = resolve("file:///etc/passwd", &base()).expect_err("refused");
        assert!(error.contains("file"), "{error}");
        let error = resolve("data:text/javascript,1", &base()).expect_err("refused");
        assert!(error.contains("data"), "{error}");
    }

    #[test]
    fn an_empty_specifier_says_so_rather_than_resolving_to_the_page() {
        assert!(resolve("   ", &base()).is_err());
    }
}
