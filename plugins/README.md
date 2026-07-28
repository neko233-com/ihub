# iHub plugin registry

`registry.json` is the maintained official discovery catalog. It points at independently versioned Git repositories under [`neko233-com`](https://github.com/neko233-com), while iHub users may also import any compatible GitHub repository directly.

`registry.lock.json` is currently in `resolved` state: all 18 catalogued official packages have an immutable release commit, a checked `plugin.json`, exact permission/capability parity, and SHA-256 integrity records for every packaged frontend or native artifact. Future packages must remain `bootstrap` and **not installable** until the same release chain is complete.

## Reproducible lock verification

Run `pnpm verify:official-plugins` from the iHub root to check the exact Git blobs in the independently checked-out local plugin repositories. It intentionally checks immutable commits rather than replacing any local working tree. Use `pnpm verify:official-plugins -- --strict-worktree` when a clean local checkout is required.

Release CI runs `node scripts/verify-official-plugin-lock.mjs --remote`, which clones every canonical GitHub repository into a temporary directory and verifies its stable tag, `plugin.json`, and every lock-listed frontend/native artifact before a release draft is created. A stale digest or a repository that is not really an independent worktree stops the release.

The catalog includes OCR, Translate, JSON/Text/Color/QR tools, Screen Recorder, Base Converter, Batch Rename, Quick Note, Clipboard History, Image Tools, Developer Tools, PDF Tools, Archive Tools, Web Actions, Screenshot, and iHub Window Layout. OCR v0.2.0 is Windows x64 only and its worker is not yet Authenticode signed. Launcher context is metadata-only for files/images and prefill-only for text. PDF and ZIP processing stays in the current WebView; Web Actions opens only reviewed HTTP(S) targets after an explicit click. A catalog URL alone is not a downloadable plugin unless its package record is marked available and its lock verifies.

## Development checkouts

All 18 projects under `official/` are independent Git checkouts in the native host's fixed, ID-only development allowlist. A trusted development installation may link their current built frontends directly; every entry falls back to its immutable Git release when that checkout is absent. `scripts/bootstrap-official-plugins.mjs` restores missing checkouts from the lock and safely fast-forwards only clean `main` branches when an update mode is explicitly requested.

The schemas in this directory make both documents machine-checkable. The registry is a catalog; it never overrides the source URL, commit, integrity, or permission record stored for an installed plugin.

See [`../docs/PLUGIN_ARCHITECTURE.md`](../docs/PLUGIN_ARCHITECTURE.md) for the GitHub import, lock, auto-update, and trust model.
