# Security Policy

## Supported versions

markdown-delight is pre-1.0; security fixes land on `main` and in the latest
`0.x` tag. There is no back-port guarantee for older tags yet.

| Version | Supported |
|---------|-----------|
| `main` / latest `0.x` | ✅ |
| older tags | ❌ |

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately to **tools@intellimass.ai** (or use GitHub's
[private vulnerability reporting](https://github.com/parker-brown-family/markdown-delight/security/advisories/new)
if enabled). Include:

- a description of the issue and its impact,
- steps to reproduce (a minimal `.md` file or theme file if relevant),
- the commit/tag you observed it on.

You'll get an acknowledgement within a few days. Once a fix is ready we'll
coordinate a disclosure timeline with you and credit you in the release notes
unless you'd prefer to stay anonymous.

## Scope notes

markdown-delight opens and renders untrusted Markdown, including inline/raw HTML
and links. Parser- or render-level issues — input that crashes, hangs, or causes
markdown-delight to read or write files outside the document you opened — are in
scope, as are theme files that can read or execute beyond the documented schema.
Comment data is stored locally under `~/.config/markdown-delight/`; report any
path that lets comment storage or export escape that directory (or land inside a
repo against the `.git/info/exclude` guard).

The vendored Zed/gpui graph is upstream's; please report renderer/framework
issues to [zed-industries/zed](https://github.com/zed-industries/zed) directly,
but feel free to flag them here too if they affect us specifically.
