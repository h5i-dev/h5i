# Vendored, pinned, and served locally

MIT-licensed production builds, kept here rather than fetched, for the reason
the fixtures exist at all: a capability suite that reaches the network measures
the network. A CDN outage, a version bump, or a machine with no route out would
each turn "does Vue mount" into a different question.

| file | what it is |
| --- | --- |
| `react.min.js`, `react-dom.min.js` | React 18 UMD production builds |
| `preact.umd.js` | Preact 10 UMD |
| `vue.global.js` | Vue 3 global build |

Each carries its own upstream licence header. Update by replacing the file and
re-running the suite: a framework that stops mounting after an upgrade is
exactly what this is here to catch.
