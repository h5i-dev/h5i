# High-impact web hypotheses

Use this catalog after normal traffic has been captured and the application's
crown-jewel actions are known. Pick hypotheses supported by observed features.
Do not run every row as a checklist.

## Identity and account lifecycle

Look for sign-up, invitation, account linking, SSO, recovery, MFA enrollment,
email change, impersonation, API-key creation, and session rotation.

High-value questions:

- Which identity is bound at issuance, presentation, and consumption of a token?
- Can an old, lower-assurance, or partially completed state perform a newer
  high-assurance action?
- Do web, mobile, legacy, and administrative APIs enforce the same issuer,
  audience, tenant, role, and lifecycle state?
- Can one actor select another actor's user, organization, MFA, or recovery
  record while supplying attacker-controlled proof?
- Does linking trust an email or provider assertion without proving control of
  the existing account?

Safe differential: compare two owned users across two owned tenants, plus an
unauthenticated control. Verify the account shown after the flow; a redirect or
token-shaped value alone is not takeover.

## Tenant and administrative boundaries

Look for organization switching, support tools, audit/export, billing, team
membership, ownership transfer, service accounts, integrations, and bulk APIs.

High-value questions:

- Is tenant context taken from the session, path, header, body, nested object,
  or a mixture?
- Does the list/read endpoint filter by tenant while update, export, bulk, or
  asynchronous endpoints trust an object ID directly?
- Can a lower role invoke an administrative function by using another API
  version, content type, batch wrapper, GraphQL mutation, or background job?
- Does support impersonation bind both the operator and target tenant, and is
  that binding rechecked at the final action?

Safe differential: use uniquely marked objects owned by A and B. Demonstrate
only the minimum read or reversible state change, then restore it.

## Hidden APIs, debug surfaces, and secondary deployments

Signals include machine-readable API contracts, internal-looking hostnames,
source maps, debug methods, staging references, generated SDKs, verbose error
types, mobile-only methods, and old versions.

High-value questions:

- Is an endpoint hidden by navigation or by authorization?
- Does a debug or administrative method rely on network placement instead of an
  application-level decision?
- Did staging, migration, regional, or legacy infrastructure keep credentials,
  identifiers, trust keys, or assumptions that production also accepts?
- Can a public client reach a method that was designed for an internal caller?

Use `recon extract`, imported contracts, and calibrated confirmation. The
existence of a hostname or candidate endpoint is not a vulnerability, and
origin discovery is allowed only when the program explicitly includes it.

## Parser and protocol differentials

Signals include gateways, CDNs, reverse proxies, request signing, multiple
languages, generated clients, XML/SOAP, URL fetchers, caches, message queues,
and content-type conversion.

High-value questions:

- Do the validator, authorizer, router, cache, serializer, and final consumer
  interpret the same bytes identically?
- Which component applies percent decoding, Unicode normalization, case folding,
  duplicate-field precedence, path cleanup, URL parsing, or default values?
- Does a signed or authenticated representation change before execution?
- Does an HTTP/2 edge translate to HTTP/1.1 upstream, and do both agree on
  request boundaries?
- Can an error path, fallback, or early response skip the component that normally
  enforces the rule?

Prefer a harmless behavioral discriminator before attempting an exploit chain.
Use an owned marker and one-variable changes. Protocol desynchronization and
shared-cache tests can affect other users; use only program-approved methods and
stop at the first safe proof of a discrepancy.

## Remote fetch, import, and generated clients

Look for webhooks, URL previews, PDF/image conversion, package installation,
OpenAPI/WSDL import, repository import, feed ingestion, SSO metadata, templates,
and connector setup.

Trace four separate decisions:

1. who may create the job;
2. how the submitted locator is parsed and validated;
3. what identity and network position fetches or interprets it;
4. what the fetched content is allowed to cause.

High-value questions:

- Are redirects, alternate schemes, DNS changes, nested imports, and generated
  method destinations revalidated by the final fetcher?
- Does fetched metadata generate code, file paths, templates, requests, or
  privileged configuration?
- Does the worker inherit cloud credentials, tenant context, or broader network
  access than the submitting user?
- Can an apparently limited file read/write reach a supported load, render,
  deploy, or extension mechanism?

Use a callback and content that you control. Do not target cloud metadata,
internal addresses, or shared file paths unless the program expressly permits
it and no safer discriminator exists.

## Upload, conversion, preview, and asynchronous workers

The web request is often only job creation. Capture job status, worker callback,
generated artifact, and cleanup as one flow.

High-value questions:

- Do validation and processing use different parsers, file names, MIME types,
  archive paths, or normalized content?
- Can a user influence a worker's command, plugin, template, dependency,
  environment, or output path?
- Is authorization checked when a job is created but omitted when the result is
  retrieved, retried, shared, or delivered?
- Does a retry or race bind the result to a different tenant or input revision?

Use inert canary files and reversible jobs. A processor crash is not a useful
impact proof and may be prohibited denial of service.

## Caches, cookies, browser boundaries, and cross-origin state

Look for shared caches, subdomain cookies, cross-origin embedding, service
workers, postMessage, WebSockets, CSP, OAuth redirects, and browser/server
disagreement.

High-value questions:

- Which request parts form the cache key, and which response parts vary outside
  it?
- Can a less-trusted sibling set a cookie that a more-trusted application reads,
  and which duplicate wins?
- Can a browser send a credentialed state-changing request that the server
  accepts without a trustworthy origin or anti-CSRF decision?
- Do redirects, errors, disk caches, or protocol upgrades change which origin,
  nonce, or state value is trusted?
- Does a self-controlled browser primitive reach a server-side feature with a
  stronger effect?

Keep cache experiments on unique owned paths and markers. For POST-CSRF,
`--permissive-cors` is required for a faithful h5i victim session and must be
authorized; without it, h5i's refusal is not evidence that the target is safe.

## Shared infrastructure and fan-out

Signals include multi-tenant workers, repository storage, build systems,
extension marketplaces, centralized identity, edge functions, global caches,
and common libraries used by many products.

High-value questions:

- Does user input cross from a tenant-local object into a shared process,
  filesystem, cache, queue, or control plane?
- Does an internal service trust metadata prepared by a less-trusted proxy or
  service without authenticating every field?
- Can one framework primitive be reached through many products or deployments?
- Is the same contract interpreted by components implemented in different
  languages or generated from different schema versions?

Shared infrastructure raises both impact and operational risk. Prove reachability
with a harmless marker, stop before accessing neighboring tenant material, and
report early.

## Turning observations into experiments

For each selected hypothesis, write:

```text
Observation:
Trust boundary:
Expected invariant:
Single mutation:
Baseline and negative controls:
Owned canary that proves the effect:
Stop condition:
Messages that will support or falsify the claim:
```

If the expected invariant and falsifying result are unclear, gather more normal
traffic before sending mutations.
