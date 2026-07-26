# Hello iHub plugin

A complete, deliberately light plugin template: plain TypeScript + Vite, no framework runtime. It validates the real package shape while remaining easy to replace with React if the plugin grows.

```powershell
Set-Location <iHub 主仓库根目录>
Set-Location examples/ihub-plugin-hello
pnpm install
pnpm dev
```

Keep copied templates under the main repository's `examples/` directory at first: the template's `file:../../plugin-sdk` dependency and Vite alias intentionally point there. For an independent repository, follow the SDK link/publish instructions in [`docs/PLUGIN_DEVELOPMENT.md`](../../docs/PLUGIN_DEVELOPMENT.md).

The Vite page runs in a browser using the SDK's in-memory development bridge. Build a distributable candidate with `pnpm build`; it retains `plugin.json` and emits `dist/index.html`.

The current iHub MVP does not provide a local-plugin-folder loader or hot reload. To exercise real host IPC, commit the built plugin to GitHub and import its repository through iHub's GitHub installer; that imports a fixed Git snapshot rather than watching this folder. See [`docs/PLUGIN_DEVELOPMENT.md`](../../docs/PLUGIN_DEVELOPMENT.md) for the host contract, permissions, native binary support, and the exact MVP/production distribution boundary.
