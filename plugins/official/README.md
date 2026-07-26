# Official plugin repository mappings

Each listed directory is reserved for the checkout of one independently maintained official plugin repository. The canonical source is GitHub, not this bootstrap copy:

| Directory | Canonical repository |
| --- | --- |
| `ihub-plugin-ocr` | `https://github.com/neko233-com/ihub-plugin-ocr` |
| `ihub-plugin-translate` | `https://github.com/neko233-com/ihub-plugin-translate` |
| `ihub-plugin-colorpick` | `https://github.com/neko233-com/ihub-plugin-colorpick` |
| `ihub-plugin-clipboard` | `https://github.com/neko233-com/ihub-plugin-clipboard` |
| `ihub-plugin-screenshot` | `https://github.com/neko233-com/ihub-plugin-screenshot` |
| `ihub-plugin-json-tools` | `https://github.com/neko233-com/ihub-plugin-json-tools` |
| `ihub-plugin-base-converter` | `https://github.com/neko233-com/ihub-plugin-base-converter` |
| `ihub-plugin-quick-note` | `https://github.com/neko233-com/ihub-plugin-quick-note` |

The `plugin.json` files currently present are bootstrap manifests that define the package and permission contract before each remote repository is initialized. They are not distributable builds because they do not contain `dist/` or native artifacts. Once a remote repository has a reviewed initial release, replace its bootstrap directory with the repository checkout (or Git submodule) and regenerate `../registry.lock.json` against an immutable release commit.

Do not use a mutable branch as a production lock. A deployment lock must contain the exact commit and SHA-256 values for the manifest and binaries.
