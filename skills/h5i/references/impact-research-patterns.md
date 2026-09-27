# Research patterns behind the impact-first workflow

This is an annotated reading map, not a payload collection. Read the original
writeups when a target shows the same architectural shape. Publication details
and program rules can change; the links are the authority.

## Repeated patterns

### Map large first-party surfaces from their own contracts

[Brutecat's StubZero writeup](https://brutecat.com/articles/google-cloud-rce/)
starts with automated fuzzing finding public debugging endpoints in an internal
Google API, then turns information exposure into production RCE. His broader
Google API research used discovery documents and structured AI-assisted probing
to make a surface too large for manual coverage tractable.

Transferable lesson: extract machine-readable contracts, normalize errors, and
give automation small typed batches. Let automation find anomalous behavior;
reserve authorization and impact judgments for controlled differential tests.

### Follow a forgotten surface into the real trust boundary

[Sam Curry and Shubs' ClubWPT Gold writeup](https://samcurry.net/hacking-clubwpt-gold)
connects a secondary deployment, exposed source and credentials, production
infrastructure, and an unauthenticated MFA-binding flaw. They stopped when the
first real customer data proved production impact.

Transferable lesson: secondary environments matter when production reuses their
identifiers, credentials, code, or trust assumptions. Treat the first foreign
sensitive record as a stop signal, not an invitation to measure the dataset.

### Search for interpretation gaps, then find a stable discriminator

[James Kettle's HTTP/1.1 desync research](https://portswigger.net/research/http1-must-die)
focuses on detecting the underlying parser discrepancy with differential
responses before attempting a brittle exploit. It then connects small protocol
differences to caches and shared reverse-proxy infrastructure.

Transferable lesson: test whether adjacent components disagree before building
a complete chain. A unique, repeatable response against proper controls is more
useful than a noisy exploit attempt.

### Reconstruct the path of trusted metadata

[Wiz Research's GitHub RCE analysis](https://www.wiz.io/blog/github-rce-vulnerability-cve-2026-3854)
traces a user-controlled git operation through several internal services and
finds a field trusted by a downstream component. AI-assisted reverse engineering
made auditing closed binaries and reconstructing the private protocol affordable.

Transferable lesson: map which component authenticates, which component prepares
metadata, and which component executes it. Internal headers and RPC fields need
their own provenance; being internal is not an authorization decision.

### Connect a weak browser primitive to a product-native execution feature

[s1r1us' Google Cloud JupyterLab writeup](https://blog.s1r1us.ninja/research/cookie-tossing-to-rce-on-google-cloud-jupyter-notebooks)
combines cookie scope, a framework-specific CSRF behavior, and Jupyter's extension
installation feature. The early observations were low impact until tied to a
legitimate server-side capability.

Transferable lesson: after finding a primitive, inventory the application's own
features that can amplify it. Prefer supported extension, import, automation,
rendering, or deployment paths over generic payload escalation.

### Generalize a framework primitive across products

[watchTowr's SOAPwn research](https://labs.watchtowr.com/soapwn-pwning-net-framework-applications-through-http-client-proxies-and-wsdl/)
starts from attacker-influenced WSDL import and studies the .NET generated-client
behavior beneath one product. The primitive then transfers to multiple enterprise
applications and reaches file-write and execution paths.

Transferable lesson: when behavior belongs to a framework or generator, search
for the reachability condition in other authorized products. Separate the
reusable primitive from each product-specific path to impact.

### Prioritize the dangerous invariant, not the visible volume

[Faav's Microsoft records writeup](https://blog.faav.net/how-i-couldve-accessed-17-trillion-microsoft-records)
describes finding a VPN-adjacent API and documentation, then a JWT verification
failure that crossed into administrative SQL access. The headline record count
describes possible blast radius; the decisive issue is the broken authentication
invariant at a privileged data interface.

Transferable lesson: prove the authorization or signature failure and the
privileged action with minimal data. Do not enumerate the affected corpus merely
to strengthen a headline.

## Technique discovery and skill organization

[PortSwigger's Top 10 Web Hacking Techniques](https://portswigger.net/research/top-10-web-hacking-techniques)
is a maintained index of innovative web research. Its yearly nominations are a
good source for new parser, cache, identity, and protocol hypotheses; use them to
recognize architecture, not to spray every new technique at every target.

[SnailSploit/Claude-Red](https://github.com/SnailSploit/Claude-Red) organizes
offensive knowledge into focused surface-specific skills. Its
[fast-checking skill](https://github.com/SnailSploit/Claude-Red/blob/main/Skills/utility/offensive-fast-checking/SKILL.md)
is a broad coverage checklist, while the IDOR, business-logic, OAuth, JWT, and
request-smuggling skills provide deeper technique catalogs.

The h5i workflow uses those catalogs selectively. Start from crown-jewel actions
and observed architecture, load the relevant deep reference, and preserve every
conclusion against captured messages. This avoids spending the request budget on
a flat checklist and prevents a tool-generated anomaly from becoming an
unsupported vulnerability claim.

## Questions to ask while reading any writeup

Extract reusable decisions rather than copying the final payload:

1. What observation caused the researcher to choose this surface?
2. What was the smallest primitive, and why did it initially look weak?
3. Which trust transition amplified it?
4. Which product-native feature supplied the final impact?
5. What control distinguished the bug from ordinary error, cache, or state?
6. What part was automated, and what required human architectural judgment?
7. Where did the researcher stop, and what evidence made further access
   unnecessary?
8. Does the primitive belong to this product, or to a reusable framework,
   parser, generated client, or protocol boundary?

Add a new hypothesis to [impact-hypotheses.md](impact-hypotheses.md) only when
the answer changes target selection or experimental design. Avoid accumulating
payload trivia that h5i cannot connect to an observed boundary and auditable
evidence.
