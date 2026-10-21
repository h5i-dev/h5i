# Showcase

## Web Applications using `h5i-app`

- [o5s](https://github.com/h5i-dev/o5s): Patch-based version control for concurrent AI agents, deployable on Cloudflare Workers.
- [h5i-db](https://github.com/Koukyosyumei/h5i-db): High-performance time-series database and backtesting engine with an `h5i-app`-based web interface.

## Benchmarks

- [h5i-app-bench](https://github.com/h5i-dev/h5i-app-bench): Benchmarks for formal verification coverage and web application performance across languages.
- [h5i-websec-bench](https://github.com/h5i-dev/h5i-websec-bench): AI-driven penetration testing benchmarks using public CTF challenges.

## Third-Party Tools

- [h5i-fastapi-starter](https://github.com/SumedhAnupSupe/h5i-fastapi-starter): A small FastAPI application with a simple HTML UI, designed to run inside an h5i box.
- [h5i-egress-advisor](https://github.com/AniketR10/h5i-egress-advisor): Reads a box's receipts and turns the egress its allowlist refused into `h5i box allow` lines to review — or a `[profile.X.net]` block for the tiers `h5i box allow` cannot reach.
- [h5i-git-hooks](https://github.com/VedantMadane/h5i-git-hooks): Git hooks that run your test suite inside an h5i box (pre-commit on the staged tree, pre-push on the tip).
- [h5i-audit-report](https://github.com/Pewpenguin/h5i-audit-report): Turns `h5i browser audit --json` into a self-contained HTML report for archiving and sharing.

## Selected vulnerabilities found by h5i

- [rustical GHSA-9cp5-7crm-9cf7](https://github.com/lennart-k/rustical/security/advisories/GHSA-9cp5-7crm-9cf7): Missing authorization checks allowed cross-user object deletion.
- [rustfs GHSA-9wgh-5r68-49p7](https://github.com/rustfs/rustfs/security/advisories/GHSA-9wgh-5r68-49p7): A Policy Evaluation That Could Loop Forever
- [Exograph #2320](https://github.com/exograph/exograph/issues/2320): Access-control bypass when protected fields were omitted during record creation.
- [OxiCloud #796](https://github.com/AtalayaLabs/OxiCloud/pull/796): IDOR vulnerability exposing other users' file metadata through music playlists.
