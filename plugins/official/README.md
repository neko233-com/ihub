# Official plugin repository mappings

Each listed directory is the local development checkout of one independently maintained official plugin repository. The canonical source is GitHub:

| Directory | Canonical repository |
| --- | --- |
| `ihub-plugin-ocr` | `https://github.com/neko233-com/ihub-plugin-ocr` |
| `ihub-plugin-translate` | `https://github.com/neko233-com/ihub-plugin-translate` |
| `ihub-plugin-colorpick` | `https://github.com/neko233-com/ihub-plugin-colorpick` |
| `ihub-plugin-clipboard` | `https://github.com/neko233-com/ihub-plugin-clipboard` |
| `ihub-plugin-screenshot` | `https://github.com/neko233-com/ihub-plugin-screenshot` |
| `ihub-plugin-image-tools` | `https://github.com/neko233-com/ihub-plugin-image-tools` |
| `ihub-plugin-json-tools` | `https://github.com/neko233-com/ihub-plugin-json-tools` |
| `ihub-plugin-text-tools` | `https://github.com/neko233-com/ihub-plugin-text-tools` |
| `ihub-plugin-base-converter` | `https://github.com/neko233-com/ihub-plugin-base-converter` |
| `ihub-plugin-quick-note` | `https://github.com/neko233-com/ihub-plugin-quick-note` |
| `ihub-plugin-screen-record` | `https://github.com/neko233-com/ihub-plugin-screen-record` |
| `ihub-plugin-batch-rename` | `https://github.com/neko233-com/ihub-plugin-batch-rename` |
| `ihub-plugin-qrcode` | `https://github.com/neko233-com/ihub-plugin-qrcode` |
| `ihub-plugin-developer-tools` | `https://github.com/neko233-com/ihub-plugin-developer-tools` |
| `ihub-plugin-window-manager` | `https://github.com/neko233-com/ihub-plugin-window-manager` |
| `ihub-plugin-pdf-tools` | `https://github.com/neko233-com/ihub-plugin-pdf-tools` |
| `ihub-plugin-archive-tools` | `https://github.com/neko233-com/ihub-plugin-archive-tools` |
| `ihub-plugin-web-actions` | `https://github.com/neko233-com/ihub-plugin-web-actions` |

All 18 directories are independent, published source repositories. The parent registry records each stable tag's resolved commit, exact manifest version and permissions, plus every served frontend/native artifact SHA-256. `scripts/bootstrap-official-plugins.mjs --locked` recreates missing checkouts without treating them as parent-repository content.

[`ihub-plugin-ocr@v0.2.0`](https://github.com/neko233-com/ihub-plugin-ocr/tree/v0.2.0) is the only package with a native artifact. Its Windows x64 `ocr-worker.exe` is hash-locked but is not Authenticode signed, so iHub presents that native boundary at install time.

Launcher file/image context carries metadata only and never grants paths or pixels; Text Tools and Translate only prefill one explicitly handed-off text value. PDF Tools and Archive Tools process selected browser `File` objects in memory. Web Actions accepts only reviewed HTTP(S) targets and opens them only after a visible click.

[`ihub-plugin-window-manager@v1.0.2`](https://github.com/neko233-com/ihub-plugin-window-manager/tree/v1.0.2) vendors the reviewed SDK for standalone builds. Its only host capability is `windowManagement`, restricted to four fixed actions on iHub's own launcher; it cannot inspect or control another application.

Do not use a mutable branch as a production lock. A deployment lock must contain the exact commit and SHA-256 values for the manifest and binaries.

## Development checkouts

Every directory is available through the native host's fixed, ID-only development allowlist. A trusted development install may link the current build as a local override; normal installs always retain the immutable Git fallback. Explicit update modes safely fast-forward only clean `main` checkouts and never reset, clean, or overwrite plugin work.
