# iHub plugin registry

`registry.json` is the maintained official discovery catalog. It points at independently versioned Git repositories under [`neko233-com`](https://github.com/neko233-com), while iHub users may also import any compatible GitHub repository directly.

`registry.lock.json` is intentionally in `bootstrap` state until those official repositories have their first signed/reviewed release. A bootstrap entry is **not installable**: the installer must resolve an immutable commit, validate `plugin.json`, calculate hashes for the manifest and every platform artifact, compare permissions, and rewrite the entry with `lockState: "resolved"` before presenting it as available.

The schemas in this directory make both documents machine-checkable. The registry is a catalog; it never overrides the source URL, commit, integrity, or permission record stored for an installed plugin.

See [`../docs/PLUGIN_ARCHITECTURE.md`](../docs/PLUGIN_ARCHITECTURE.md) for the GitHub import, lock, auto-update, and trust model.
