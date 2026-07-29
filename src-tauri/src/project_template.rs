use std::{
    fs::{self, OpenOptions},
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::models::PluginProjectCreated;

const MAX_PLUGIN_ID_LENGTH: usize = 63;

/// Creates a new standalone TypeScript/Vite plugin project below a user-selected
/// directory. The final project directory is reserved with `create_dir` before
/// any template file is written, so an existing project is never replaced.
pub fn create_plugin_project(
    parent_directory: &str,
    plugin_id: &str,
) -> Result<PluginProjectCreated, String> {
    let parent = resolve_parent_directory(parent_directory)?;
    validate_plugin_id(plugin_id)?;

    let destination = parent.join(plugin_id);
    if destination.exists() {
        return Err(format!(
            "A file or directory named '{plugin_id}' already exists in {}. iHub did not overwrite it.",
            parent.display()
        ));
    }

    match fs::create_dir(&destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!(
                "A file or directory named '{plugin_id}' already exists in {}. iHub did not overwrite it.",
                parent.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "Could not reserve a new project directory at {}: {error}",
                destination.display()
            ));
        }
    }

    if let Err(error) = write_project_template(&destination, plugin_id) {
        return Err(format!(
            "Could not finish the plugin template: {error}. No existing files were overwritten. The incomplete project was kept at {} for inspection.",
            destination.display()
        ));
    }

    let project_path = display_path(&destination);
    Ok(PluginProjectCreated {
        project_path: project_path.clone(),
        plugin_id: plugin_id.to_owned(),
        next_steps: vec![
            format!("cd \"{project_path}\""),
            "Read README.md; this starter has no dependency on an unpublished iHub SDK."
                .to_owned(),
            "pnpm install".to_owned(),
            "pnpm dev (optional browser preview while you work)".to_owned(),
            "pnpm build (also runs a read-only pre-link check; it never runs the optional worker)"
                .to_owned(),
            "Use this project's absolute directory in iHub Developer → Link local plugin only after the frontend build succeeds."
                .to_owned(),
            "After each edit, run pnpm build and close/reopen the linked plugin frontend to load the new dist files."
                .to_owned(),
            "For GitHub import, commit plugin.json, dist/, and every declared bin/ artifact; the importer never installs or builds the repository."
                .to_owned(),
            "The Rust worker sample is optional; enable it only after building a real target binary and declaring that exact artifact in plugin.json."
                .to_owned(),
        ],
    })
}

fn resolve_parent_directory(parent_directory: &str) -> Result<PathBuf, String> {
    if parent_directory.trim().is_empty() {
        return Err("Choose a parent directory for the new plugin project.".to_owned());
    }

    let requested = PathBuf::from(parent_directory);
    if !requested.is_absolute() {
        return Err("The plugin project parent directory must be an absolute path.".to_owned());
    }

    let canonical = requested.canonicalize().map_err(|error| {
        format!(
            "Could not resolve plugin project parent directory '{}': {error}",
            requested.display()
        )
    })?;
    if !canonical.is_dir() {
        return Err(format!(
            "Plugin project parent '{}' is not a directory.",
            canonical.display()
        ));
    }
    Ok(canonical)
}

fn validate_plugin_id(plugin_id: &str) -> Result<(), String> {
    let length = plugin_id.len();
    let valid_characters = plugin_id
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    let starts_with_lowercase = plugin_id
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase());
    let has_empty_segment = plugin_id.split('-').any(str::is_empty);

    if !(3..=MAX_PLUGIN_ID_LENGTH).contains(&length)
        || !valid_characters
        || !starts_with_lowercase
        || has_empty_segment
    {
        return Err(
            "Plugin ID must be 3–63 lowercase kebab-case characters (for example: ihub-plugin-my-tool)."
                .to_owned(),
        );
    }
    Ok(())
}

fn write_project_template(root: &Path, plugin_id: &str) -> Result<(), String> {
    for directory in ["src", "public", "worker", "worker/src", "scripts", "docs"] {
        fs::create_dir_all(root.join(directory))
            .map_err(|error| format!("Could not create {directory} directory: {error}"))?;
    }

    let display_name = display_name_for(plugin_id);
    let manifest = serde_json::to_string_pretty(&json!({
        "schemaVersion": 1,
        "id": plugin_id,
        "name": display_name,
        "version": "0.1.0",
        "description": format!("{} for iHub.", display_name),
        "icon": "public/icon.png",
        "license": "MIT",
        "engines": {
            "ihub": ">=0.1.0",
            "api": "^1.0.0"
        },
        "entry": {
            "frontend": "dist/index.html"
        },
        "activationEvents": ["onCommand:open"],
        "contributes": {
            "commands": [{
                "id": "open",
                "title": format!("Open {}", display_name),
                "subtitle": "Open this TypeScript + Vite plugin starter",
                "keywords": ["typescript", "vite", "plugin", "starter"],
                "icon": "public/icon.png"
            }]
        },
        "permissions": {}
    }))
    .map_err(|error| format!("Could not serialize plugin manifest: {error}"))?;
    let package = serde_json::to_string_pretty(&json!({
        "name": plugin_id,
        "private": true,
        "version": "0.1.0",
        "type": "module",
        "scripts": {
            "dev": "vite",
            "build": "tsc --noEmit && vite build && node ./scripts/verify-plugin.mjs",
            "check": "tsc --noEmit",
            "verify": "node ./scripts/verify-plugin.mjs",
            "preview": "vite preview",
            "build:worker:windows": "powershell -NoProfile -ExecutionPolicy Bypass -File ./scripts/build-worker.ps1",
            "build:worker:mac": "sh ./scripts/build-worker.sh"
        },
        "devDependencies": {
            "typescript": "^5.7.3",
            "vite": "^8.0.0"
        }
    }))
    .map_err(|error| format!("Could not serialize package.json: {error}"))?;

    write_new_file(root, "plugin.json", &format!("{manifest}\n"))?;
    write_new_file(root, "package.json", &format!("{package}\n"))?;
    write_new_file(root, "tsconfig.json", TSCONFIG)?;
    write_new_file(root, "vite.config.ts", VITE_CONFIG)?;
    write_new_file(root, "index.html", &index_html(&display_name))?;
    write_new_file(root, "src/main.ts", &main_source(&display_name))?;
    write_new_file(root, "src/ihub-bridge.ts", IHUB_BRIDGE_SOURCE)?;
    write_new_file(root, "src/style.css", STYLE_SOURCE)?;
    let placeholder_icon = plugin_placeholder_png()?;
    write_new_binary_file(root, "public/icon.png", &placeholder_icon)?;
    write_new_file(root, "worker/Cargo.toml", WORKER_CARGO_TOML)?;
    write_new_file(root, "worker/src/main.rs", WORKER_MAIN_SOURCE)?;
    write_new_file(root, "scripts/build-worker.ps1", BUILD_WORKER_PS1)?;
    write_new_file(root, "scripts/build-worker.sh", BUILD_WORKER_SH)?;
    write_new_file(root, "scripts/verify-plugin.mjs", VERIFY_PLUGIN_MJS)?;
    write_new_file(root, "docs/JSONL_RPC.md", JSONL_RPC_DOCUMENTATION)?;
    write_new_file(
        root,
        "docs/ENABLE_NATIVE_WORKER.md",
        NATIVE_WORKER_DOCUMENTATION,
    )?;
    write_new_file(root, ".gitignore", GITIGNORE)?;
    write_new_file(root, "README.md", &readme(&display_name, plugin_id))?;
    Ok(())
}

fn write_new_file(root: &Path, relative_path: &str, contents: &str) -> Result<(), String> {
    let path = root.join(relative_path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| format!("Could not write {}: {error}", path.display()))
}

fn write_new_binary_file(root: &Path, relative_path: &str, contents: &[u8]) -> Result<(), String> {
    let path = root.join(relative_path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
    file.write_all(contents)
        .map_err(|error| format!("Could not write {}: {error}", path.display()))
}

fn plugin_placeholder_png() -> Result<Vec<u8>, String> {
    const SIZE: u32 = 128;
    let mut image = image::RgbaImage::new(SIZE, SIZE);

    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let corner_x = if x < 32 {
            32_i32 - x as i32
        } else if x >= 96 {
            x as i32 - 95
        } else {
            0
        };
        let corner_y = if y < 32 {
            32_i32 - y as i32
        } else if y >= 96 {
            y as i32 - 95
        } else {
            0
        };
        let in_rounded_tile = (10..118).contains(&x)
            && (10..118).contains(&y)
            && corner_x * corner_x + corner_y * corner_y <= 22 * 22;

        *pixel = if !in_rounded_tile {
            image::Rgba([0, 0, 0, 0])
        } else if (33..61).contains(&x) && (33..61).contains(&y) {
            image::Rgba([218, 226, 236, 255])
        } else if (67..95).contains(&x) && (33..61).contains(&y) {
            image::Rgba([166, 181, 199, 255])
        } else if (33..61).contains(&x) && (67..95).contains(&y) {
            image::Rgba([139, 157, 178, 255])
        } else if ((76..86).contains(&x) && (67..95).contains(&y))
            || ((67..95).contains(&x) && (76..86).contains(&y))
        {
            image::Rgba([241, 245, 249, 255])
        } else {
            image::Rgba([75, 85, 99, 255])
        };
    }

    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut output, image::ImageFormat::Png)
        .map_err(|error| {
            format!("Could not encode the generic plugin placeholder icon: {error}")
        })?;
    Ok(output.into_inner())
}

fn display_name_for(plugin_id: &str) -> String {
    let words = plugin_id.strip_prefix("ihub-plugin-").unwrap_or(plugin_id);
    words
        .split('-')
        .map(title_case_ascii)
        .collect::<Vec<_>>()
        .join(" ")
}

fn title_case_ascii(word: &str) -> String {
    let mut characters = word.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    format!("{}{}", first.to_ascii_uppercase(), characters.as_str())
}

#[cfg(test)]
fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    #[cfg(target_os = "windows")]
    {
        if let Some(unc_path) = path.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc_path}");
        }
        if let Some(normal_path) = path.strip_prefix(r"\\?\") {
            return normal_path.to_owned();
        }
    }
    path.into_owned()
}

fn index_html(display_name: &str) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <meta name="theme-color" content="#0c1020" />
    <title>{display_name} · iHub Plugin</title>
  </head>
  <body>
    <main id="app"></main>
    <script type="module" src="/src/main.ts"></script>
  </body>
</html>
"##
    )
}

fn main_source(display_name: &str) -> String {
    let display_name =
        serde_json::to_string(display_name).unwrap_or_else(|_| "\"iHub Plugin\"".to_owned());
    format!(
        r##"import {{ createIHubBridge, hasIHubHost }} from "./ihub-bridge";
import manifest from "../plugin.json";
import "./style.css";

const pluginName = {display_name};
const app = document.querySelector<HTMLDivElement>("#app");

if (!app) {{
  throw new Error("The plugin root element is missing.");
}}

const isPreview = !hasIHubHost();
const ihub = createIHubBridge(manifest.id);

app.innerHTML = [
  '<section class="card" aria-labelledby="title">',
  '<p class="eyebrow">' + (isPreview ? "Browser preview" : "Connected to iHub") + "</p>",
  '<h1 id="title">' + pluginName + "</h1>",
  "<p>This starter is immediately linkable after its TypeScript build. A Rust JSONL worker sample is included but deliberately disabled until you build and declare a real target binary.</p>",
  '<button id="action" type="button">Test frontend bridge</button>',
  '<p id="status" class="status" role="status">Ready.</p>',
  "</section>",
].join("");

const action = document.querySelector<HTMLButtonElement>("#action");
const status = document.querySelector<HTMLParagraphElement>("#status");

if (!action || !status) {{
  throw new Error("The starter plugin UI did not initialize.");
}}

function showStatus(message: string): void {{
  status.textContent = message;
}}

action.addEventListener("click", () => {{
  void ihub.log("info", "Starter action ran", {{ preview: isPreview }});
  showStatus("Frontend bridge call sent. Build the frontend, then link this project from iHub. See README before enabling the optional Rust worker.");
}});

async function activate(): Promise<void> {{
  await ihub.registerCommand(
    {{
      id: "open",
      title: "Open " + pluginName,
      subtitle: "Open this plugin's starter view",
      keywords: ["plugin", "starter"],
    }},
    async () => {{
      const message = pluginName + " is ready to customize.";
      showStatus(message);
      return {{ message, close: false }};
    }},
  );

  await ihub.ready();
  await ihub.log("info", "Plugin activated", {{ preview: isPreview }});
}}

void activate().catch((error) => {{
  showStatus("Plugin error: " + (error instanceof Error ? error.message : String(error)));
  console.error(error);
}});
"##
    )
}

fn readme(display_name: &str, plugin_id: &str) -> String {
    format!(
        r#"# {display_name}

`{plugin_id}` is a standalone TypeScript + Vite iHub plugin scaffold generated by iHub.

The generated frontend intentionally has **no dependency on an unpublished npm package**. `src/ihub-bridge.ts` is a small, vendored implementation of iHub's iframe `postMessage` contract, so `pnpm install` and `pnpm build` work immediately. You can migrate to the official `@ihub/plugin-sdk` after it is published or when you deliberately link a local checkout.

## Develop the TypeScript frontend

```powershell
pnpm install
pnpm dev
```

Build the frontend for iHub:

```powershell
pnpm build
```

`pnpm build` ends with `scripts/verify-plugin.mjs`. That check reads only this project's `plugin.json`, built frontend entry, declared artwork, and any binary paths you explicitly declared. It does **not** install packages, launch the preview server, execute plugin code, or run a native worker. You can run the same read-only check on its own with `pnpm verify`.

`public/icon.png` is deliberately a neutral plugin placeholder, not the iHub application logo and not a publishable brand identity. Replace it with your own PNG, JPEG, or WebP artwork before publishing. Keep artwork package-relative, at most 2 MiB, no larger than 1024×1024 or 1,048,576 pixels, and never use SVG or a symbolic link. Static command icons belong in `plugin.json`; runtime `registerCommand` calls cannot send icon paths or image payloads. The host may ignore an unusable command icon from a legacy package and show its safe fallback, while an invalid top-level `icon`/`logo` still prevents that package from loading.

The supplied `.gitignore` intentionally leaves `dist/` visible to Git. iHub's GitHub importer reads the files committed at the chosen ref and never runs your package manager or build scripts, so a publishable plugin must include its built frontend output.

## Fast local debug loop

1. Review `plugin.json`, `src/main.ts`, and the generated bridge before running anything.
2. Run `pnpm install` yourself, then use `pnpm dev` for an optional browser-only preview while building the UI.
3. Run `pnpm build`. It must finish successfully before iHub can load `dist/index.html`.
4. In iHub, open **Plugin Center → Developer**. The project-creation result pre-fills this exact directory in **Link local plugin**; alternatively choose the directory yourself. Linking records a canonical path only—it does not copy the project or execute scripts.
5. Rebuild after each edit, then close and reopen the linked plugin frontend to load the updated `dist/` files. Local links intentionally do not use a watcher or HMR, so iHub never starts a development server for you.

The **Open project folder** button in iHub is an explicit convenience action only. It opens the generated directory in Finder/Explorer; it does not run a terminal command or any project file.

## Host sub-input

The vendored bridge includes the same visible-surface sub-input primitive as the SDK. The input is rendered by iHub above the isolated iframe; the callback stays in this page and receives bounded `{{ text }}` updates. Hidden search runtimes cannot use it, and closing or replacing the plugin surface removes it:

```ts
await ihub.subInput.set(
  ({{ text }}) => console.log("filter", text),
  "Search this plugin",
  true,
);

await ihub.subInput.setValue("initial value");
await ihub.subInput.remove();
```

Migrate to `@ihub/plugin-sdk` when you need the deliberately limited `window.utools` / `window.rubick` source-compatibility projection. The generated bridge never exposes Node.js, Electron remote, filesystem paths, arbitrary processes, preload scripts, or shell execution.

## Browser screen picker (optional)

If this plugin records a user-selected browser display, add `"screenCapture": true` under `permissions` in `plugin.json`. iHub delegates cross-origin display capture only to a native-validated visible Surface lease; hidden search runtimes and undeclared plugins receive no delegation. The generated `ihub.screenCapture` bridge only holds iHub visible while the browser's own `getDisplayMedia()` picker temporarily moves focus. Neither mechanism grants screen pixels, bypasses the browser/OS picker or OS permission, or exposes native capture. Browser-only preview cannot prove desktop permission or successful frame delivery.

Start the lease request, then call `getDisplayMedia()` **synchronously in the same click handler without awaiting the lease**. Awaiting an iframe/host round trip first can consume Chromium's transient user activation. Always release the lease in `finally`, and treat a failed acquisition as non-fatal to the browser picker:

```ts
const leasePromise = ihub.screenCapture.acquireFocusLease().catch(() => null);
const streamPromise = navigator.mediaDevices.getDisplayMedia({{ video: true, audio: false }});
try {{
  const stream = await streamPromise;
  // use the user-approved stream
}} finally {{
  const lease = await leasePromise;
  if (lease) await ihub.screenCapture.releaseFocusLease(lease.leaseId).catch(() => undefined);
}}
```

## System cursor pixel (optional)

If this plugin needs the color under the system cursor, add `"cursorColor": true` under `permissions` in `plugin.json`, then call `await ihub.cursorColor.sampleOnce()` from a visible plugin view. iHub, not the plugin iframe, shows a confirmation before each request. After a fixed two-second countdown it reads one cursor pixel and returns only `{{ hex, rgb }}`—never a screenshot, coordinates, a display ID, or another window's contents. Hidden runtimes cannot use this API. The bridge keeps this one request open for up to two minutes so an initial macOS Screen Recording permission prompt can finish; it is still not a background task.

Treat a rejected request as a normal user choice and keep a manual color-input fallback. Do not call it automatically on startup, in a timer, or from a background worker: the host only permits the visible, user-confirmed one-shot flow.

## Explicit launcher context (optional)

For a visible “send this selection to this plugin” action, add only the needed `launcherContext.text`, `launcherContext.files`, and/or `launcherContext.image` flags in `plugin.json`. iHub does not automatically send clipboard contents, paths, or image pixels. The trusted parent supplies a short-lived opaque `launcherContext` only on the command event the user chose; consume it once inside that handler with `ihub.launcherContext.consume(invocation.launcherContext.contextId)`. Text is bounded, files have metadata/opaque handles without paths, and images have PNG metadata/opaque handles without bytes. Never log, persist, upload, or replay the context ID.

## Optional Rust JSONL worker

The `worker/` folder and build scripts are an optional native-worker sample. The generated `plugin.json` deliberately declares **no** backend binary and no `nativeApi` permission, so the frontend can be built and linked immediately. iHub validates every declared binary at link/import time; do not claim a platform until its exact artifact has been built and tested.

When you are ready to add a native capability, build the target matching the machine where iHub will run it. The script copies Cargo's release output into `bin/<platform>/`:

```powershell
# Windows x64
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-worker.ps1 -Target windows-x86_64

# Windows ARM64
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-worker.ps1 -Target windows-aarch64
```

```sh
# macOS Apple Silicon (or omit the argument to infer the current Mac)
sh ./scripts/build-worker.sh darwin-aarch64

# macOS Intel
sh ./scripts/build-worker.sh darwin-x86_64
```

Cross-compiling needs the matching Rust target and platform linker/SDK. The scripts fail instead of pretending a binary was produced. Run `cargo test --manifest-path worker/Cargo.toml` to exercise the worker's protocol handler.

After the artifact exists, follow `docs/ENABLE_NATIVE_WORKER.md` for the exact v1 manifest change, command declaration, and optional TypeScript bridge call. Add `nativeApi: true` only when the frontend itself calls the declared worker. `docs/JSONL_RPC.md` describes the one-request/one-response contract. Do not declare the other Windows/macOS targets until their binaries are genuinely built, tested, and included.

## Test in iHub

Keep `plugin.json` at the repository root. Build the frontend, then use iHub's **Plugin Center → Developer → Link local plugin** action with this project's absolute directory. If you opted into the worker, build its host-platform artifact before linking. iHub stores only a canonical path and reads your project in place; after each new build, close and reopen the plugin frontend to load the updated `dist/` files. This link is not HMR or a file watcher.

## Publish and import through GitHub

iHub's GitHub importer reads a committed Git snapshot only. It does **not** run `pnpm install`, `pnpm build`, package scripts, Git hooks, or the optional worker. Before creating a release tag:

1. Run `pnpm build`, then run `pnpm verify` once more after any manifest or worker change.
2. Commit `plugin.json`, the generated `dist/` directory, and every `bin/<target>/` artifact declared in `plugin.json`.
3. Review the committed files and publish a release tag or full commit ID. A branch is convenient during development but is mutable.
4. In iHub choose **GitHub import** and enter `owner/repo@v1.2.0` (or a full GitHub URL with `#ref`). iHub resolves that ref and locks the resulting commit for this installation.

Do not depend on an ignored build directory, a local `node_modules/`, or a CI-only build step being present during import: none of those are run or copied by the importer.

## Trust model

The TypeScript bridge exposes only host calls accepted by iHub. The Rust worker is **not sandboxed**: it runs with the same user authority as iHub. Manifest `permissions` gate frontend bridge calls, not arbitrary native-process behavior. Do not add a binary dependency or distribute a worker you would not trust as a normal desktop executable.

## Next steps

- Replace `src/main.ts` and `src/style.css` with your product UI.
- Replace the generic `public/icon.png` placeholder with your own plugin artwork before publishing.
- Extend `src/ihub-bridge.ts` or migrate to the published SDK for more host APIs.
- Add static commands, search providers, settings, and only the permissions you need in `plugin.json`.
- Enable and replace the optional starter worker with OCR, FFmpeg, system automation, or another native capability only after building the target artifact.
- Read the iHub plugin development guide for manifest, worker protocol, and trust-model details.
"#
    )
}

const IHUB_BRIDGE_SOURCE: &str = r###"export interface CommandDefinition {
  id: string;
  title: string;
  subtitle?: string;
  keywords?: string[];
  execution?: "frontend" | "native";
  /**
   * Runtime registrations intentionally have no icon field. Declare artwork
   * on the static command in plugin.json; the bridge never forwards image
   * paths or payloads supplied by running frontend code.
   */
  readonly icon?: never;
}

export interface CommandInvocation {
  requestId: string;
  commandId: string;
  input?: unknown;
  context?: unknown;
  /** Opaque, one-shot context attached by a visible iHub launcher action. */
  launcherContext?: LauncherContextInvocation;
}

export interface LauncherContextInvocation {
  contextId: string;
  expiresInMs: number;
}

export interface LauncherContextFileMetadata {
  handleId: string;
  name: string;
  kind: "file" | "folder";
  size?: number;
}

export interface LauncherContextImageHandle {
  handleId: string;
  name: string;
  mimeType: "image/png";
  width: number;
  height: number;
}

export interface LauncherContextPayload {
  text?: string;
  files: LauncherContextFileMetadata[];
  image?: LauncherContextImageHandle;
}

export interface CommandResult {
  message?: string;
  close?: boolean;
  [key: string]: unknown;
}

/** A host-owned, short-lived guard against launcher auto-hide during a picker. */
export interface ScreenCaptureFocusLease {
  leaseId: string;
  expiresInMs: number;
}

export interface ScreenCaptureFocusLeaseRelease {
  released: boolean;
}

/** One user-confirmed system cursor pixel; no coordinates or screen image are exposed. */
export interface CursorColorSample {
  hex: string;
  rgb: string;
}

/** File metadata returned to the iframe; canonical paths stay in the host. */
export interface FilesystemSelectedFile {
  name: string;
  size: number;
}

export type FilesystemFileSelection =
  | { cancelled: true }
  | { cancelled: false; grantId: string; files: FilesystemSelectedFile[] };

export interface NativeCommandResult {
  pluginId: string;
  commandId: string;
  success: boolean;
  exitCode: number | null;
  stdout: string;
  stderr: string;
  output?: unknown;
}

type CommandHandler = (invocation: CommandInvocation) => CommandResult | Promise<CommandResult>;
type SubInputChangeHandler = (change: { text: string }) => void | Promise<void>;

export interface IHubBridge {
  registerCommand(
    definition: CommandDefinition,
    handler: CommandHandler,
  ): Promise<() => Promise<void>>;
  readonly subInput: {
    /** Controls the trusted input in the current visible iHub plugin surface. */
    set(
      onChange: SubInputChangeHandler,
      placeholder?: string,
      focus?: boolean,
    ): Promise<boolean>;
    remove(): Promise<boolean>;
    setValue(value: string): Promise<boolean>;
  };
  readonly screenCapture: {
    /**
     * Start this request and call browser getDisplayMedia() immediately,
     * without awaiting it, so the click's transient activation survives.
     */
    acquireFocusLease(): Promise<ScreenCaptureFocusLease>;
    releaseFocusLease(leaseId: string): Promise<ScreenCaptureFocusLeaseRelease>;
  };
  readonly cursorColor: {
    /** Requires cursorColor: true and a visible, host-confirmed user action. */
    sampleOnce(): Promise<CursorColorSample>;
  };
  readonly launcherContext: {
    /**
     * Consume only a contextId supplied on this command invocation. It never
     * reads the clipboard, resolves a local path, or exposes image bytes.
     */
    consume(contextId: string): Promise<LauncherContextPayload>;
  };
  readonly filesystem: {
    /** Requires filesystem.read: ["user-selected"] in plugin.json. */
    selectFiles(): Promise<FilesystemFileSelection>;
  };
  readonly native: {
    /** Requires nativeApi and a declared backend binary in plugin.json. */
    runCommand(options: { commandId: string; input?: unknown; fileGrantId?: string }): Promise<NativeCommandResult>;
  };
  ready(): Promise<void>;
  log(level: "debug" | "info" | "warn" | "error", message: string, details?: unknown): Promise<void>;
}

const REQUEST_CHANNEL = "ihub-plugin-bridge/v1";
const RESPONSE_CHANNEL = "ihub-host-bridge/v1";
const CALL_TIMEOUT_MS = 30_000;
// The host confirmation and an initial macOS Screen Recording prompt may take
// longer than ordinary Bridge calls. This only extends the iframe wait.
const CURSOR_COLOR_CALL_TIMEOUT_MS = 2 * 60 * 1_000;
// The host still decides the command deadline from plugin.json. This grace
// only keeps the iframe attached long enough to receive a valid host result
// for a declared native task (the host maximum is 30 minutes).
const NATIVE_COMMAND_CALL_TIMEOUT_MS = 30 * 60 * 1_000 + 10_000;
const MAX_SUB_INPUT_PLACEHOLDER_LENGTH = 160;
const MAX_SUB_INPUT_VALUE_LENGTH = 4_096;

interface PendingCall {
  resolve(value: unknown): void;
  reject(reason: Error): void;
  timeout: number;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object";
}

export function hasIHubHost(): boolean {
  return typeof window !== "undefined" && window.parent !== window;
}

/**
 * A dependency-free bridge for generated projects. It mirrors the public
 * iframe protocol so the project can build before @ihub/plugin-sdk is
 * published. The parent validates the iframe source and reconstructs the
 * plugin identity plus a host-held lease; this helper never exposes Tauri
 * APIs. Its `pluginId` field is routing metadata, not an authority token.
 */
export function createIHubBridge(pluginId: string): IHubBridge {
  const preview = !hasIHubHost();
  const pending = new Map<string, PendingCall>();
  const commandHandlers = new Map<string, CommandHandler>();
  const settings = new Map<string, unknown>();
  let subInputActive = false;
  let subInputHandler: SubInputChangeHandler | null = null;
  let sequence = 0;

  const notifySubInput = (text: string): void => {
    const handler = subInputHandler;
    if (!handler) {
      return;
    }
    void Promise.resolve(handler({ text })).catch((error) => {
      console.error("[iHub sub-input]", error);
    });
  };

  const previewCall = (method: string, params: Record<string, unknown>): unknown => {
    if (method === "ui.subInput.set") {
      subInputActive = true;
      return true;
    }
    if (method === "ui.subInput.remove") {
      subInputActive = false;
      return true;
    }
    if (method === "ui.subInput.setValue") {
      if (!subInputActive) {
        return false;
      }
      notifySubInput(String(params.value ?? ""));
      return true;
    }
    if (method === "filesystem.selectFiles") {
      throw new Error("File selection requires the iHub desktop host.");
    }
    if (method === "native.runCommand") {
      throw new Error("Native plugin commands require the iHub desktop host.");
    }
    if (method === "screenCapture.acquireFocusLease" || method === "screenCapture.releaseFocusLease") {
      throw new Error("Screen-capture focus leases require the iHub desktop host.");
    }
    if (method === "cursorColor.sampleOnce") {
      throw new Error("System cursor color sampling requires the iHub desktop host.");
    }
    if (method === "launcherContext.consume") {
      throw new Error("Launcher context transfers require an explicit iHub desktop-host action.");
    }
    if (method === "settings.get") {
      return settings.get(String(params.key)) ?? params.fallback ?? null;
    }
    if (method === "settings.set") {
      settings.set(String(params.key), params.value);
      return { saved: true };
    }
    if (method === "log") {
      console.info("[iHub preview]", params.message, params.details ?? null);
    }
    return { ok: true };
  };

  const call = <T>(
    method: string,
    params: Record<string, unknown> = {},
    timeoutMs = CALL_TIMEOUT_MS,
  ): Promise<T> => {
    if (preview) {
      try {
        return Promise.resolve(previewCall(method, params) as T);
      } catch (error) {
        return Promise.reject(error);
      }
    }

    return new Promise<T>((resolve, reject) => {
      const id = "starter-" + Date.now().toString(36) + "-" + (sequence++).toString(36);
      const timeout = window.setTimeout(() => {
        pending.delete(id);
        reject(new Error("iHub host call timed out."));
      }, timeoutMs);
      pending.set(id, {
        resolve: (value) => resolve(value as T),
        reject: (reason) => reject(reason),
        timeout,
      });
      window.parent.postMessage(
        {
          channel: REQUEST_CHANNEL,
          type: "call",
          id,
          request: { pluginId, method, params },
        },
        "*",
      );
    });
  };

  const completeCommand = async (payload: unknown): Promise<void> => {
    if (!isRecord(payload) || typeof payload.requestId !== "string" || typeof payload.commandId !== "string") {
      return;
    }

    const handler = commandHandlers.get(payload.commandId);
    if (!handler) {
      await call("commands.complete", {
        requestId: payload.requestId,
        ok: false,
        result: null,
        error: "Unknown command: " + payload.commandId,
      });
      return;
    }

    try {
      const result = await handler({
        requestId: payload.requestId,
        commandId: payload.commandId,
        input: payload.input,
        context: payload.context,
        launcherContext: isRecord(payload.launcherContext)
          && typeof payload.launcherContext.contextId === "string"
          && typeof payload.launcherContext.expiresInMs === "number"
          ? {
            contextId: payload.launcherContext.contextId,
            expiresInMs: payload.launcherContext.expiresInMs,
          }
          : undefined,
      });
      await call("commands.complete", {
        requestId: payload.requestId,
        ok: true,
        result: result ?? {},
        error: null,
      });
    } catch (error) {
      await call("commands.complete", {
        requestId: payload.requestId,
        ok: false,
        result: null,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  };

  window.addEventListener("message", (event: MessageEvent<unknown>) => {
    if (!preview && event.source !== window.parent) {
      return;
    }
    if (!isRecord(event.data) || event.data.channel !== RESPONSE_CHANNEL) {
      return;
    }

    if (event.data.type === "event" && typeof event.data.name === "string") {
      if (event.data.name === "ihub://plugin/" + pluginId + "/command") {
        void completeCommand(event.data.payload);
      }
      if (
        event.data.name === "ihub://plugin/" + pluginId + "/event/subInput.change"
        && isRecord(event.data.payload)
        && typeof event.data.payload.text === "string"
        && event.data.payload.text.length <= MAX_SUB_INPUT_VALUE_LENGTH
      ) {
        notifySubInput(event.data.payload.text);
      }
      return;
    }

    if (event.data.type !== "response" || typeof event.data.id !== "string") {
      return;
    }
    const pendingCall = pending.get(event.data.id);
    if (!pendingCall) {
      return;
    }
    pending.delete(event.data.id);
    window.clearTimeout(pendingCall.timeout);
    if (event.data.ok === true) {
      pendingCall.resolve(event.data.result);
    } else {
      pendingCall.reject(
        new Error(typeof event.data.error === "string" ? event.data.error : "iHub host call failed."),
      );
    }
  });

  return {
    async registerCommand(definition, handler) {
      if (commandHandlers.has(definition.id)) {
        throw new Error("Command is already registered: " + definition.id);
      }
      commandHandlers.set(definition.id, handler);
      try {
        const { icon: _ignoredIcon, ...hostDefinition } = definition as CommandDefinition & {
          icon?: unknown;
        };
        await call("commands.register", { definition: hostDefinition });
      } catch (error) {
        commandHandlers.delete(definition.id);
        throw error;
      }
      return async () => {
        commandHandlers.delete(definition.id);
        await call("commands.unregister", { commandId: definition.id });
      };
    },
    subInput: {
      async set(onChange, placeholder = "", focus = true) {
        if (typeof onChange !== "function") {
          throw new Error("Sub-input change handler must be a function.");
        }
        if (
          typeof placeholder !== "string"
          || placeholder.length > MAX_SUB_INPUT_PLACEHOLDER_LENGTH
        ) {
          throw new Error("Sub-input placeholder must be at most 160 characters.");
        }
        if (typeof focus !== "boolean") {
          throw new Error("Sub-input focus must be a boolean.");
        }
        const previousHandler = subInputHandler;
        subInputHandler = onChange;
        try {
          const accepted = await call<boolean>("ui.subInput.set", { placeholder, focus });
          if (accepted !== true) {
            subInputHandler = previousHandler;
            return false;
          }
          return true;
        } catch (error) {
          subInputHandler = previousHandler;
          throw error;
        }
      },
      async remove() {
        const removed = await call<boolean>("ui.subInput.remove");
        if (removed === true) {
          subInputHandler = null;
          return true;
        }
        return false;
      },
      setValue(value) {
        if (typeof value !== "string" || value.length > MAX_SUB_INPUT_VALUE_LENGTH) {
          return Promise.reject(new Error("Sub-input value must be at most 4096 characters."));
        }
        return call<boolean>("ui.subInput.setValue", { value });
      },
    },
    screenCapture: {
      acquireFocusLease: () => call<ScreenCaptureFocusLease>("screenCapture.acquireFocusLease"),
      releaseFocusLease: (leaseId) =>
        call<ScreenCaptureFocusLeaseRelease>("screenCapture.releaseFocusLease", { leaseId }),
    },
    cursorColor: {
      sampleOnce: () => call<CursorColorSample>(
        "cursorColor.sampleOnce",
        {},
        CURSOR_COLOR_CALL_TIMEOUT_MS,
      ),
    },
    launcherContext: {
      consume: (contextId) => call<LauncherContextPayload>("launcherContext.consume", { contextId }),
    },
    filesystem: {
      selectFiles: () => call<FilesystemFileSelection>("filesystem.selectFiles"),
    },
    native: {
      runCommand: (options) => call<NativeCommandResult>(
        "native.runCommand",
        options,
        NATIVE_COMMAND_CALL_TIMEOUT_MS,
      ),
    },
    async ready() {
      await call("lifecycle.ready");
    },
    async log(level, message, details) {
      await call("log", { level, message, details: details ?? null });
    },
  };
}
"###;

const WORKER_CARGO_TOML: &str = r#"[package]
name = "ihub-plugin-worker"
version = "0.1.0"
edition = "2021"
rust-version = "1.77"
publish = false

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
"#;

const WORKER_MAIN_SOURCE: &str = r###"use std::{
    env,
    io::{self, BufRead, Write},
};

use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: String,
    #[serde(default)]
    id: Value,
    #[serde(default)]
    method: String,
    #[serde(default)]
    params: Value,
}

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());

    for line in stdin.lock().lines() {
        let response = match line {
            Ok(line) => handle_line(&line),
            Err(error) => error_response(Value::Null, -32700, format!("Could not read stdin: {error}")),
        };

        // iHub expects exactly one JSON-RPC response per stdout line. Keep all
        // diagnostics on stderr so a logging statement cannot corrupt IPC.
        if serde_json::to_writer(&mut writer, &response).is_err()
            || writeln!(writer).is_err()
            || writer.flush().is_err()
        {
            eprintln!("iHub worker could not write a JSON-RPC response.");
            break;
        }
    }
}

fn handle_line(line: &str) -> Value {
    match serde_json::from_str::<JsonRpcRequest>(line) {
        Ok(request) => handle_request(request),
        Err(error) => error_response(Value::Null, -32700, format!("Invalid JSON-RPC request: {error}")),
    }
}

fn handle_request(request: JsonRpcRequest) -> Value {
    if request.jsonrpc != "2.0" {
        return error_response(request.id, -32600, "jsonrpc must be exactly '2.0'.");
    }

    match request.method.as_str() {
        "worker-echo" => json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": {
                "message": "Rust starter worker received the request.",
                "params": request.params,
                "pluginId": env::var("IHUB_PLUGIN_ID").ok(),
                "commandId": env::var("IHUB_COMMAND_ID").ok(),
                "platform": {
                    "os": env::consts::OS,
                    "arch": env::consts::ARCH
                }
            }
        }),
        _ => error_response(
            request.id,
            -32601,
            format!("Method '{}' is not implemented by this worker.", request.method),
        ),
    }
}

fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into()
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::{handle_request, JsonRpcRequest};

    #[test]
    fn worker_echo_returns_json_rpc_result() {
        let response = handle_request(JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: json!(7),
            method: "worker-echo".to_owned(),
            params: json!({ "text": "hello" }),
        });

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 7);
        assert_eq!(response["result"]["params"]["text"], "hello");
    }

    #[test]
    fn unknown_method_returns_json_rpc_error() {
        let response = handle_request(JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: json!("request-1"),
            method: "missing".to_owned(),
            params: Value::Null,
        });

        assert_eq!(response["error"]["code"], -32601);
    }
}
"###;

const VERIFY_PLUGIN_MJS: &str = r###"import { existsSync, lstatSync, readFileSync, statSync } from "node:fs";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(scriptDirectory, "..");
const PLUGIN_ID = /^[a-z0-9][a-z0-9-]{1,62}$/;
const ARTWORK_CONTROL_CHARACTERS = /[\u0000-\u001f\u007f-\u009f]/;
const WINDOWS_DEVICE_NAME = /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\..*)?$/i;
const MAX_COMMANDS = 64;
const MAX_ARTWORK_CANDIDATES = 32;
const MAX_ARTWORK_BYTES = 2 * 1024 * 1024;
const MAX_ARTWORK_EDGE = 1024;
const MAX_ARTWORK_PIXELS = 1024 * 1024;
const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const SUPPORTED_TARGETS = new Set([
  "windows-x86_64",
  "windows-aarch64",
  "darwin-x86_64",
  "darwin-aarch64",
]);

function fail(message) {
  throw new Error(message);
}

function isRecord(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function projectFile(label, declaredPath) {
  if (typeof declaredPath !== "string" || !declaredPath.trim()) {
    fail(label + " must be a non-empty relative file path.");
  }
  if (isAbsolute(declaredPath)) {
    fail(label + " must stay relative to the plugin project.");
  }

  const resolved = resolve(projectRoot, declaredPath);
  const pathFromRoot = relative(projectRoot, resolved);
  if (
    !pathFromRoot
    || pathFromRoot === ".."
    || pathFromRoot.startsWith(".." + sep)
    || isAbsolute(pathFromRoot)
  ) {
    fail(label + " must stay inside the plugin project.");
  }
  if (!existsSync(resolved) || !statSync(resolved).isFile()) {
    fail(label + " is missing: " + declaredPath);
  }
  return resolved;
}

function safeArtworkComponents(label, declaredPath) {
  if (typeof declaredPath !== "string" || !declaredPath) {
    fail(label + " must be a non-empty package-relative artwork path.");
  }
  if (
    isAbsolute(declaredPath) ||
    declaredPath.startsWith("/") ||
    declaredPath.startsWith("\\") ||
    declaredPath.includes(":") ||
    ARTWORK_CONTROL_CHARACTERS.test(declaredPath)
  ) {
    fail(label + " must be a safe package-relative artwork path.");
  }

  const components = declaredPath.split(/[\\/]/);
  const unsafeComponent = components.find(
    (component) =>
      !component ||
      component === "." ||
      component === ".." ||
      component.endsWith(".") ||
      component.endsWith(" ") ||
      WINDOWS_DEVICE_NAME.test(component),
  );
  if (unsafeComponent !== undefined) {
    fail(
      label +
        " contains an empty/dot component, Windows device name, or trailing dot/space.",
    );
  }
  return components;
}

function readArtwork(label, declaredPath, artworkCandidates) {
  const components = safeArtworkComponents(label, declaredPath);
  const candidate = components.join("/");
  artworkCandidates.add(candidate);
  if (artworkCandidates.size > MAX_ARTWORK_CANDIDATES) {
    fail(
      "plugin.json may reference at most " +
        MAX_ARTWORK_CANDIDATES +
        " distinct artwork files.",
    );
  }

  const resolved = resolve(projectRoot, ...components);
  const pathFromRoot = relative(projectRoot, resolved);
  if (
    !pathFromRoot ||
    pathFromRoot === ".." ||
    pathFromRoot.startsWith(".." + sep) ||
    isAbsolute(pathFromRoot)
  ) {
    fail(label + " must stay inside the plugin project.");
  }

  let current = projectRoot;
  for (const [index, component] of components.entries()) {
    current = resolve(current, component);
    let metadata;
    try {
      metadata = lstatSync(current);
    } catch {
      fail(label + " is missing: " + declaredPath);
    }
    if (metadata.isSymbolicLink()) {
      fail(label + " must not use a symbolic link: " + declaredPath);
    }
    if (index < components.length - 1 && !metadata.isDirectory()) {
      fail(label + " has a non-directory path component: " + declaredPath);
    }
    if (index === components.length - 1 && !metadata.isFile()) {
      fail(label + " must reference a regular file: " + declaredPath);
    }
  }

  const bytes = readFileSync(resolved);
  if (bytes.length > MAX_ARTWORK_BYTES) {
    fail(label + " must not exceed 2 MiB.");
  }
  const dimensions = decodeArtworkDimensions(label, bytes);
  const [width, height] = dimensions;
  if (
    width < 1 ||
    height < 1 ||
    width > MAX_ARTWORK_EDGE ||
    height > MAX_ARTWORK_EDGE ||
    width * height > MAX_ARTWORK_PIXELS
  ) {
    fail(
      label +
        " dimensions must be at most 1024×1024 and 1,048,576 total pixels.",
    );
  }
  return resolved;
}

function decodeArtworkDimensions(label, bytes) {
  if (bytes.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE)) {
    return decodePngDimensions(label, bytes);
  }
  if (bytes.length >= 4 && bytes[0] === 0xff && bytes[1] === 0xd8) {
    return decodeJpegDimensions(label, bytes);
  }
  if (
    bytes.length >= 12 &&
    bytes.toString("ascii", 0, 4) === "RIFF" &&
    bytes.toString("ascii", 8, 12) === "WEBP"
  ) {
    return decodeWebpDimensions(label, bytes);
  }
  fail(label + " must contain PNG, JPEG, or WebP bytes; SVG is not accepted.");
}

function decodePngDimensions(label, bytes) {
  let offset = 8;
  let dimensions;
  let sawImageData = false;
  let sawEnd = false;

  while (offset + 12 <= bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const chunkEnd = offset + 12 + length;
    if (chunkEnd > bytes.length) {
      fail(label + " contains a truncated PNG chunk.");
    }
    const chunkType = bytes.toString("ascii", offset + 4, offset + 8);
    if (offset === 8 && (chunkType !== "IHDR" || length !== 13)) {
      fail(label + " has an invalid PNG header.");
    }
    if (chunkType === "IHDR") {
      if (dimensions || length !== 13) {
        fail(label + " has an invalid PNG header.");
      }
      dimensions = [
        bytes.readUInt32BE(offset + 8),
        bytes.readUInt32BE(offset + 12),
      ];
    } else if (chunkType === "IDAT") {
      sawImageData = true;
    } else if (chunkType === "IEND") {
      if (length !== 0 || chunkEnd !== bytes.length) {
        fail(label + " has an invalid PNG end chunk.");
      }
      sawEnd = true;
      break;
    }
    offset = chunkEnd;
  }

  if (!dimensions || !sawImageData || !sawEnd) {
    fail(label + " is a malformed or incomplete PNG.");
  }
  return dimensions;
}

function decodeJpegDimensions(label, bytes) {
  if (bytes.length < 12 || bytes.at(-2) !== 0xff || bytes.at(-1) !== 0xd9) {
    fail(label + " is a malformed or incomplete JPEG.");
  }

  let offset = 2;
  while (offset + 1 < bytes.length) {
    if (bytes[offset] !== 0xff) {
      fail(label + " contains an invalid JPEG marker.");
    }
    while (offset < bytes.length && bytes[offset] === 0xff) {
      offset += 1;
    }
    const marker = bytes[offset++];
    if (marker === 0xd9) {
      break;
    }
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) {
      continue;
    }
    if (offset + 2 > bytes.length) {
      fail(label + " contains a truncated JPEG segment.");
    }
    const segmentLength = bytes.readUInt16BE(offset);
    if (segmentLength < 2 || offset + segmentLength > bytes.length) {
      fail(label + " contains an invalid JPEG segment.");
    }
    const isStartOfFrame =
      (marker >= 0xc0 && marker <= 0xc3) ||
      (marker >= 0xc5 && marker <= 0xc7) ||
      (marker >= 0xc9 && marker <= 0xcb) ||
      (marker >= 0xcd && marker <= 0xcf);
    if (isStartOfFrame) {
      if (segmentLength < 8) {
        fail(label + " contains an invalid JPEG frame header.");
      }
      return [bytes.readUInt16BE(offset + 5), bytes.readUInt16BE(offset + 3)];
    }
    if (marker === 0xda) {
      fail(label + " has JPEG image data before a valid frame header.");
    }
    offset += segmentLength;
  }
  fail(label + " does not contain a supported JPEG frame.");
}

function decodeWebpDimensions(label, bytes) {
  const riffLength = bytes.readUInt32LE(4) + 8;
  if (riffLength !== bytes.length) {
    fail(label + " contains an invalid or truncated WebP RIFF container.");
  }

  let offset = 12;
  let canvasDimensions;
  while (offset + 8 <= bytes.length) {
    const chunkType = bytes.toString("ascii", offset, offset + 4);
    const chunkLength = bytes.readUInt32LE(offset + 4);
    const payload = offset + 8;
    const chunkEnd = payload + chunkLength;
    if (chunkEnd > bytes.length) {
      fail(label + " contains a truncated WebP chunk.");
    }

    if (chunkType === "VP8X") {
      if (chunkLength !== 10) {
        fail(label + " contains an invalid WebP extended header.");
      }
      canvasDimensions = [
        1 + bytes.readUIntLE(payload + 4, 3),
        1 + bytes.readUIntLE(payload + 7, 3),
      ];
    } else if (chunkType === "VP8 ") {
      if (
        chunkLength < 10 ||
        bytes[payload + 3] !== 0x9d ||
        bytes[payload + 4] !== 0x01 ||
        bytes[payload + 5] !== 0x2a
      ) {
        fail(label + " contains an invalid WebP VP8 frame.");
      }
      const dimensions = [
        bytes.readUInt16LE(payload + 6) & 0x3fff,
        bytes.readUInt16LE(payload + 8) & 0x3fff,
      ];
      if (
        canvasDimensions &&
        (canvasDimensions[0] !== dimensions[0] || canvasDimensions[1] !== dimensions[1])
      ) {
        fail(label + " has inconsistent WebP dimensions.");
      }
      return canvasDimensions || dimensions;
    } else if (chunkType === "VP8L") {
      if (chunkLength < 5 || bytes[payload] !== 0x2f) {
        fail(label + " contains an invalid WebP lossless frame.");
      }
      const dimensions = [
        1 + bytes[payload + 1] + ((bytes[payload + 2] & 0x3f) << 8),
        1 +
          (bytes[payload + 2] >> 6) +
          (bytes[payload + 3] << 2) +
          ((bytes[payload + 4] & 0x0f) << 10),
      ];
      if (
        canvasDimensions &&
        (canvasDimensions[0] !== dimensions[0] || canvasDimensions[1] !== dimensions[1])
      ) {
        fail(label + " has inconsistent WebP dimensions.");
      }
      return canvasDimensions || dimensions;
    }

    offset = chunkEnd + (chunkLength & 1);
  }
  fail(label + " does not contain a supported WebP image frame.");
}

try {
  const artworkCandidates = new Set();
  const manifestPath = projectFile("plugin.json", "plugin.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (!isRecord(manifest)) {
    fail("plugin.json must contain a JSON object.");
  }
  if (manifest.schemaVersion !== 1) {
    fail("plugin.json schemaVersion must be 1.");
  }
  if (typeof manifest.id !== "string" || !PLUGIN_ID.test(manifest.id)) {
    fail("plugin.json id must be lowercase kebab-case.");
  }
  if (!isRecord(manifest.permissions)) {
    fail("plugin.json permissions must be an object, even when it is empty.");
  }
  if (manifest.icon !== undefined && manifest.logo !== undefined) {
    fail("plugin.json must declare only one of icon or logo.");
  }
  if (manifest.icon !== undefined) {
    readArtwork("icon", manifest.icon, artworkCandidates);
  }
  if (manifest.logo !== undefined) {
    readArtwork("logo", manifest.logo, artworkCandidates);
  }

  const frontend = manifest.entry && manifest.entry.frontend;
  const frontendFile = projectFile("entry.frontend", frontend);
  if (!relative(projectRoot, dirname(frontendFile))) {
    fail("entry.frontend must live in a dedicated build directory such as dist/index.html.");
  }

  const commands = manifest.contributes && manifest.contributes.commands;
  if (commands !== undefined && !Array.isArray(commands)) {
    fail("contributes.commands must be an array when present.");
  }
  if ((commands || []).length > MAX_COMMANDS) {
    fail("contributes.commands must contain at most " + MAX_COMMANDS + " entries.");
  }
  let nativeCommands = 0;
  for (const [index, command] of (commands || []).entries()) {
    if (!isRecord(command) || typeof command.id !== "string" || !command.id.trim()) {
      fail("contributes.commands[" + index + "] needs an id.");
    }
    if (command.icon !== undefined) {
      readArtwork(
        "contributes.commands[" + index + "].icon",
        command.icon,
        artworkCandidates,
      );
    }
    if (command.execution !== undefined && command.execution !== "frontend" && command.execution !== "native") {
      fail("contributes.commands[" + index + "].execution must be frontend or native.");
    }
    if (command.run !== undefined) {
      if (!isRecord(command.run) || command.execution !== "native") {
        fail("contributes.commands[" + index + "].run requires execution: native.");
      }
      if (Object.keys(command.run).some((key) => key !== "timeoutMs")) {
        fail("contributes.commands[" + index + "].run only supports timeoutMs.");
      }
      if (!Number.isInteger(command.run.timeoutMs) || command.run.timeoutMs < 1_000 || command.run.timeoutMs > 30 * 60 * 1_000) {
        fail("contributes.commands[" + index + "].run.timeoutMs must be an integer from 1000 to 1800000.");
      }
    }
    if (command.execution === "native") {
      nativeCommands += 1;
    }
  }

  const backend = manifest.backend;
  let nativeArtifacts = 0;
  if (backend !== undefined && !isRecord(backend)) {
    fail("backend must be an object when present.");
  }
  if (backend) {
    if (backend.binary !== undefined) {
      fail("Use v1 backend.binaries entries instead of the legacy backend.binary field.");
    }
    if (backend.protocol !== "jsonl-rpc-v1") {
      fail("backend.protocol must be jsonl-rpc-v1.");
    }
    if (!Array.isArray(backend.binaries) || !backend.binaries.length) {
      fail("backend.binaries must contain at least one built target artifact.");
    }
    const declaredTargets = new Set();
    for (const [index, binary] of backend.binaries.entries()) {
      if (!isRecord(binary) || typeof binary.target !== "string" || !SUPPORTED_TARGETS.has(binary.target)) {
        fail("backend.binaries[" + index + "] needs a supported Windows/macOS target.");
      }
      if (declaredTargets.has(binary.target)) {
        fail("backend.binaries declares target " + binary.target + " more than once.");
      }
      declaredTargets.add(binary.target);
      projectFile("backend.binaries[" + index + "].path", binary.path);
      nativeArtifacts += 1;
    }
  } else if (nativeCommands) {
    fail("A command with execution: native requires a declared backend binary.");
  }
  if (manifest.permissions.nativeApi === true && !backend) {
    fail("permissions.nativeApi requires a declared backend binary.");
  }

  console.log("✓ v1 plugin.json: " + manifest.id);
  console.log("✓ frontend build: " + frontend);
  console.log(
    nativeArtifacts
      ? "✓ declared native artifacts: " + nativeArtifacts
      : "✓ frontend-only starter: no native worker is declared",
  );
  console.log("\nReady for iHub Developer → Link local plugin:");
  console.log(projectRoot);
  console.log("Static verification only: no package install, plugin code, or native worker was run.");
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  console.error("iHub pre-link check failed: " + message);
  console.error("Fix the project, run pnpm build again, then link it from iHub.");
  process.exitCode = 1;
}
"###;

const BUILD_WORKER_PS1: &str = r###"[CmdletBinding()]
param(
  [ValidateSet("windows-x86_64", "windows-aarch64", "darwin-x86_64", "darwin-aarch64")]
  [string]$Target
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-HostTarget {
  $architecture = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString().ToLowerInvariant()
  switch ($architecture) {
    "x64" { $architecture = "x86_64" }
    "arm64" { $architecture = "aarch64" }
    default { throw "Unsupported CPU architecture: $architecture" }
  }

  if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::Windows
  )) {
    return "windows-$architecture"
  }
  if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::OSX
  )) {
    return "darwin-$architecture"
  }
  throw "This starter only packages Windows and macOS workers."
}

if (-not $Target) {
  $Target = Get-HostTarget
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  throw "Rust cargo was not found. Install Rust with rustup, then retry."
}

$targets = @{
  "windows-x86_64" = "x86_64-pc-windows-msvc"
  "windows-aarch64" = "aarch64-pc-windows-msvc"
  "darwin-x86_64" = "x86_64-apple-darwin"
  "darwin-aarch64" = "aarch64-apple-darwin"
}
$rustTarget = $targets[$Target]
if (-not $rustTarget) {
  throw "Unsupported iHub target: $Target"
}

$root = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $root "worker/Cargo.toml"
& cargo build --release --manifest-path $manifest --target $rustTarget
if ($LASTEXITCODE -ne 0) {
  throw "cargo build failed for $rustTarget"
}

$binaryName = "ihub-plugin-worker"
if ($Target.StartsWith("windows-")) {
  $binaryName += ".exe"
}
$source = Join-Path $root ("worker/target/{0}/release/{1}" -f $rustTarget, $binaryName)
if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
  throw "cargo completed but the expected worker was not found: $source"
}
$destinationDirectory = Join-Path $root ("bin/{0}" -f $Target)
New-Item -ItemType Directory -Force -Path $destinationDirectory | Out-Null
$destination = Join-Path $destinationDirectory $binaryName
Copy-Item -LiteralPath $source -Destination $destination -Force
Write-Host "Built iHub worker: $destination"
"###;

const BUILD_WORKER_SH: &str = r###"#!/usr/bin/env sh
set -eu

target="${1:-}"
if [ -z "$target" ]; then
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os/$arch" in
    Darwin/arm64) target="darwin-aarch64" ;;
    Darwin/x86_64) target="darwin-x86_64" ;;
    *)
      echo "Pass darwin-aarch64 or darwin-x86_64 explicitly; this script only packages macOS workers." >&2
      exit 1
      ;;
  esac
fi

case "$target" in
  windows-x86_64) rust_target="x86_64-pc-windows-msvc"; binary_name="ihub-plugin-worker.exe" ;;
  windows-aarch64) rust_target="aarch64-pc-windows-msvc"; binary_name="ihub-plugin-worker.exe" ;;
  darwin-x86_64) rust_target="x86_64-apple-darwin"; binary_name="ihub-plugin-worker" ;;
  darwin-aarch64) rust_target="aarch64-apple-darwin"; binary_name="ihub-plugin-worker" ;;
  *)
    echo "Unsupported iHub target: $target" >&2
    exit 1
    ;;
esac

if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust cargo was not found. Install Rust with rustup, then retry." >&2
  exit 1
fi

script_dir="$(CDPATH= cd "$(dirname "$0")" && pwd)"
root_dir="$(CDPATH= cd "$script_dir/.." && pwd)"

cargo build --release --manifest-path "$root_dir/worker/Cargo.toml" --target "$rust_target"

source_path="$root_dir/worker/target/$rust_target/release/$binary_name"
if [ ! -f "$source_path" ]; then
  echo "cargo completed but the expected worker was not found: $source_path" >&2
  exit 1
fi

destination_dir="$root_dir/bin/$target"
mkdir -p "$destination_dir"
cp "$source_path" "$destination_dir/$binary_name"
chmod +x "$destination_dir/$binary_name"
printf '%s\n' "Built iHub worker: $destination_dir/$binary_name"
"###;

const JSONL_RPC_DOCUMENTATION: &str = r#"# iHub worker JSON Lines RPC

The generated Rust worker uses `jsonl-rpc-v1`. iHub starts the platform-matched binary, writes one JSON-RPC 2.0 object followed by a newline to stdin, waits for one JSON object followed by a newline on stdout, then exits the worker process for that invocation.

## Request and response

For the manifest command `worker-echo`, iHub sends:

```json
{"jsonrpc":"2.0","id":"1","method":"worker-echo","params":{"example":true}}
```

The starter responds with:

```json
{"jsonrpc":"2.0","id":"1","result":{"message":"Rust starter worker received the request.","params":{"example":true}}}
```

Unknown methods return JSON-RPC error `-32601`. Invalid JSON or invalid JSON-RPC envelopes return a JSON-RPC error instead of crashing the host request.

## Rules

- Write **only** complete JSON-RPC objects to stdout, one line per response. Logs, progress, tracing, and third-party-library output must use stderr.
- Preserve the request `id` in the response. iHub currently sends one request per worker process, but code should still treat `id` as opaque JSON.
- Send file paths, offsets, and compact structured data in `params`. Do not put images, videos, or other large binary payloads in JSON Lines.
- The current host provides `IHUB_PLUGIN_ID` and `IHUB_COMMAND_ID`; request data arrives only through stdin JSON Lines, not an environment variable. Do not assume the working directory is the plugin root; treat any future data-directory or protocol environment variables as optional.
- A native command gets a 60-second host deadline by default. Its manifest may explicitly set `run.timeoutMs` from 1,000 to 1,800,000 milliseconds; the host terminates the worker on that deadline. This is not a background-job or progress API.
- Handle cancellation, timeouts, and child-process cleanup in real workers. In particular, a worker that starts FFmpeg or another child process must own and clean up that child: this v1 host only terminates the declared worker process. The starter is intentionally a small one-request/one-response example.

The native worker is not sandboxed. Treat every executable and bundled native dependency as code that will run with the user's desktop permissions.
"#;

const NATIVE_WORKER_DOCUMENTATION: &str = r#"# Enable the optional native worker

The generated project starts as a frontend-only plugin on purpose. Do not add a backend declaration until the target artifact actually exists, has been tested, and is ready to be distributed. iHub does not build workers during a local link or GitHub import.

## 1. Build and test one target

Run the matching script from the project root, then test the worker protocol:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-worker.ps1 -Target windows-x86_64
cargo test --manifest-path worker/Cargo.toml
```

```sh
sh ./scripts/build-worker.sh darwin-aarch64
cargo test --manifest-path worker/Cargo.toml
```

The scripts copy a real artifact to `bin/<target>/ihub-plugin-worker[.exe]`. Do not declare a target that was not built and tested on a compatible machine.

## 2. Declare the built worker in `plugin.json`

For v1, declare every shipped executable in `backend.binaries`. Merge these changes with the existing manifest. Keep the existing `open` command; add the `worker-echo` command and activation event shown below. The example declares Windows x64 only—add macOS or ARM64 entries only after their own artifacts exist.

```json
{
  "activationEvents": ["onCommand:open", "onCommand:worker-echo"],
  "contributes": {
    "commands": [
      { "id": "open", "title": "Open My Feature", "execution": "frontend" },
      {
        "id": "worker-echo",
        "title": "Run worker echo",
        "execution": "native",
        "run": { "timeoutMs": 900000 }
      }
    ]
  },
  "permissions": {},
  "backend": {
    "protocol": "jsonl-rpc-v1",
    "binaries": [
      {
        "target": "windows-x86_64",
        "path": "bin/windows-x86_64/ihub-plugin-worker.exe"
      }
    ]
  }
}
```

`execution: "native"` allows iHub to launch the declared worker for that command. It does not by itself expose the worker to the TypeScript frontend. Omit `run` for the compatible 60-second deadline, or set `run.timeoutMs` (1,000–1,800,000) only for a foreground task whose duration genuinely needs it. The timeout is part of the reviewed native command declaration, so a routine Git update cannot silently extend it.

## 3. Call the worker from TypeScript only when needed

If your frontend calls the worker through the bridge, change `permissions` to include `"nativeApi": true` and make an explicit user-driven call:

```ts
const result = await ihub.native.runCommand({
  commandId: "worker-echo",
  input: { message: "hello" },
});
```

`nativeApi` is unnecessary when the worker is started only by a native launcher command. The generated bridge waits up to the host's maximum plus a small response grace for `native.runCommand`; it never grants a longer host deadline. This is not a sandbox or a general process-spawn permission: the bundled worker still runs with the user's desktop authority. There is no v1 cancel, background continuation, progress, or process-tree cleanup API, so a worker that starts FFmpeg must manage its own child processes.

## 4. Verify, link, then publish

1. Run `pnpm verify`; it checks the manifest, built frontend, and each declared worker artifact without executing plugin code or the worker.
2. Run `pnpm build` after frontend changes, then close and reopen the linked plugin frontend to reload `dist/`.
3. Before GitHub import, commit `plugin.json`, `dist/`, and every declared `bin/<target>/` artifact. The importer only reads the committed snapshot and never runs build scripts.

Keep stdout strictly for one JSON-RPC response per line. Read `docs/JSONL_RPC.md` before replacing the sample worker with OCR, FFmpeg, or another native implementation.
"#;

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "useDefineForClassFields": true,
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "skipLibCheck": true,
    "moduleResolution": "Bundler",
    "resolveJsonModule": true,
    "isolatedModules": true,
    "noEmit": true,
    "strict": true
  },
  "include": ["src", "vite.config.ts"]
}
"#;

const VITE_CONFIG: &str = r#"import { defineConfig } from "vite";

export default defineConfig({
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
"#;

const STYLE_SOURCE: &str = r##":root {
  color: #edf3ff;
  background: #0b1020;
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  font-synthesis: none;
}

* {
  box-sizing: border-box;
}

body {
  min-width: 320px;
  min-height: 100vh;
  margin: 0;
  display: grid;
  place-items: center;
  background:
    radial-gradient(circle at 16% 12%, rgb(92 112 255 / 26%), transparent 27rem),
    radial-gradient(circle at 86% 82%, rgb(56 221 184 / 17%), transparent 24rem),
    #0b1020;
}

.card {
  width: min(560px, calc(100vw - 32px));
  padding: 34px;
  border: 1px solid rgb(255 255 255 / 13%);
  border-radius: 24px;
  background: linear-gradient(145deg, rgb(30 38 67 / 94%), rgb(13 18 34 / 94%));
  box-shadow: 0 30px 78px rgb(0 0 0 / 38%), inset 0 1px rgb(255 255 255 / 8%);
}

.eyebrow {
  margin: 0;
  color: #78e5ce;
  font-size: 0.78rem;
  font-weight: 750;
  letter-spacing: 0.09em;
  text-transform: uppercase;
}

h1 {
  margin: 14px 0 10px;
  font-size: clamp(2rem, 7vw, 3.4rem);
  letter-spacing: -0.05em;
}

p {
  color: #bdc8e4;
  line-height: 1.6;
}

button {
  margin-top: 12px;
  padding: 12px 17px;
  border: 0;
  border-radius: 12px;
  color: #09101e;
  font: inherit;
  font-weight: 750;
  cursor: pointer;
  background: linear-gradient(135deg, #aebaff, #6de8cb);
}

.status {
  min-height: 1.6em;
  margin-bottom: 0;
  color: #8deed7;
  font-weight: 650;
}
"##;

const GITIGNORE: &str = "node_modules/\nworker/target/\n.DS_Store\n.env\n.env.*\n!.env.example\n";

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    use super::{create_plugin_project, validate_plugin_id};

    fn temporary_parent(label: &str) -> PathBuf {
        let parent = std::env::temp_dir().join(format!(
            "ihub-project-template-{label}-{}",
            super::unique_suffix()
        ));
        fs::create_dir(&parent).expect("temporary parent should be created");
        parent
    }

    fn verifier_project(label: &str) -> (PathBuf, PathBuf) {
        let parent = temporary_parent(label);
        let parent_text = parent.to_string_lossy().into_owned();
        create_plugin_project(&parent_text, "ihub-plugin-demo").expect("template");
        let project = parent.join("ihub-plugin-demo");
        fs::create_dir(project.join("dist")).expect("dist directory");
        fs::write(project.join("dist/index.html"), "<!doctype html>")
            .expect("built frontend placeholder");
        (parent, project)
    }

    fn run_verifier(project: &Path) -> Output {
        Command::new("node")
            .arg("scripts/verify-plugin.mjs")
            .current_dir(project)
            .output()
            .expect("Node.js must be available to exercise the generated verifier")
    }

    fn verifier_output(output: &Output) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    fn manifest(project: &Path) -> serde_json::Value {
        let contents =
            fs::read_to_string(project.join("plugin.json")).expect("generated manifest contents");
        serde_json::from_str(&contents).expect("generated manifest JSON")
    }

    fn write_manifest(project: &Path, manifest: &serde_json::Value) {
        let contents = serde_json::to_string_pretty(manifest).expect("manifest serialization");
        fs::write(project.join("plugin.json"), format!("{contents}\n")).expect("updated manifest");
    }

    fn assert_verifier_fails(project: &Path, expected: &str) {
        let output = run_verifier(project);
        let combined = verifier_output(&output);
        assert!(
            !output.status.success(),
            "verifier unexpectedly succeeded:\n{combined}"
        );
        assert!(
            combined.contains(expected),
            "expected verifier output to contain {expected:?}, got:\n{combined}"
        );
    }

    #[test]
    fn creates_a_linkable_frontend_template_with_an_optional_rust_worker_sample() {
        let parent = temporary_parent("create");
        let parent_text = parent.to_string_lossy().into_owned();

        let result = create_plugin_project(&parent_text, "ihub-plugin-demo").expect("template");
        let project = parent.join("ihub-plugin-demo");
        assert_eq!(
            PathBuf::from(&result.project_path)
                .canonicalize()
                .expect("reported project path should resolve"),
            project
                .canonicalize()
                .expect("expected project path should resolve")
        );
        assert_eq!(result.plugin_id, "ihub-plugin-demo");
        assert!(project.join("plugin.json").is_file());
        assert!(project.join("src/main.ts").is_file());
        assert!(project.join("src/ihub-bridge.ts").is_file());
        assert!(project.join("public/icon.png").is_file());
        assert!(
            fs::read(project.join("public/icon.png"))
                .expect("generated icon")
                .starts_with(b"\x89PNG\r\n\x1a\n"),
            "the generated manifest artwork must be a real raster PNG"
        );
        let generated_icon = fs::read(project.join("public/icon.png")).expect("generated icon");
        assert_ne!(
            generated_icon.as_slice(),
            include_bytes!("../icons/128x128.png"),
            "the plugin starter must not reuse the branded iHub app logo"
        );
        let decoded_icon =
            image::load_from_memory(&generated_icon).expect("generated placeholder should decode");
        assert_eq!((decoded_icon.width(), decoded_icon.height()), (128, 128));
        assert!(project.join("worker/Cargo.toml").is_file());
        assert!(project.join("worker/src/main.rs").is_file());
        assert!(project.join("scripts/build-worker.ps1").is_file());
        assert!(project.join("scripts/build-worker.sh").is_file());
        assert!(project.join("scripts/verify-plugin.mjs").is_file());
        assert!(project.join("docs/JSONL_RPC.md").is_file());
        assert!(project.join("docs/ENABLE_NATIVE_WORKER.md").is_file());

        let package = fs::read_to_string(project.join("package.json")).expect("package contents");
        assert!(!package.contains("@ihub/plugin-sdk"));
        assert!(!package.contains("file:"));
        assert!(package.contains("build:worker:windows"));
        assert!(package.contains("build:worker:mac"));
        assert!(package.contains("verify-plugin.mjs"));
        assert!(package.contains("\"verify\""));

        let manifest = fs::read_to_string(project.join("plugin.json")).expect("manifest contents");
        assert!(!manifest.contains("@ihub/plugin-sdk"));
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("generated manifest should be JSON");
        assert!(manifest.get("backend").is_none());
        assert_eq!(manifest["icon"], "public/icon.png");
        assert_eq!(manifest["contributes"]["commands"][0]["id"], "open");
        assert_eq!(
            manifest["contributes"]["commands"][0]["icon"],
            "public/icon.png"
        );
        assert_eq!(manifest["activationEvents"][0], "onCommand:open");

        let bridge = fs::read_to_string(project.join("src/ihub-bridge.ts"))
            .expect("generated frontend bridge source");
        assert!(bridge.contains("screenCapture"));
        assert!(bridge.contains("subInput"));
        assert!(bridge.contains("ui.subInput.set"));
        assert!(bridge.contains("MAX_SUB_INPUT_VALUE_LENGTH"));
        assert!(bridge.contains("acquireFocusLease"));
        assert!(bridge.contains("releaseFocusLease"));
        assert!(bridge.contains("cursorColor"));
        assert!(bridge.contains("sampleOnce"));
        assert!(bridge.contains("launcherContext"));
        assert!(bridge.contains("launcherContext.consume"));
        assert!(bridge.contains("LauncherContextPayload"));
        assert!(bridge.contains("CURSOR_COLOR_CALL_TIMEOUT_MS"));
        assert!(bridge.contains("selectFiles"));
        assert!(bridge.contains("native.runCommand"));
        assert!(bridge.contains("NATIVE_COMMAND_CALL_TIMEOUT_MS"));
        assert!(bridge.contains("require the iHub desktop host"));
        assert!(bridge.contains("Runtime registrations intentionally have no icon field"));
        assert!(bridge.contains("definition: hostDefinition"));

        let readme = fs::read_to_string(project.join("README.md")).expect("generated README");
        assert!(readme.contains("Host sub-input"));
        assert!(readme.contains("window.utools"));
        assert!(readme.contains("never exposes Node.js"));
        assert!(readme.contains("screenCapture"));
        assert!(readme.contains("without awaiting the lease"));
        assert!(readme.contains("System cursor pixel"));
        assert!(readme.contains("cursorColor"));
        assert!(readme.contains("up to two minutes"));
        assert!(readme.contains("Explicit launcher context"));
        assert!(readme.contains("launcherContext.text"));
        assert!(readme.contains("deliberately declares **no** backend binary"));
        assert!(readme.contains("Fast local debug loop"));
        assert!(readme.contains("Open project folder"));
        assert!(readme.contains("GitHub importer reads a committed Git snapshot only"));
        assert!(readme.contains("dist/` visible to Git"));
        assert!(readme.contains("neutral plugin placeholder"));
        assert!(readme.contains("Replace it with your own PNG, JPEG, or WebP artwork"));
        assert!(readme.contains("legacy package"));

        let gitignore =
            fs::read_to_string(project.join(".gitignore")).expect("generated gitignore");
        assert!(!gitignore.lines().any(|line| line.trim() == "dist/"));
        assert!(gitignore.contains("node_modules/"));
        assert!(gitignore.contains("worker/target/"));

        let verifier = fs::read_to_string(project.join("scripts/verify-plugin.mjs"))
            .expect("generated pre-link verifier");
        assert!(verifier.contains("Static verification only"));
        assert!(verifier.contains("entry.frontend"));
        assert!(verifier.contains("SUPPORTED_TARGETS"));
        assert!(verifier.contains("schemaVersion must be 1"));
        assert!(verifier.contains("execution: native"));
        assert!(verifier.contains("MAX_COMMANDS = 64"));
        assert!(verifier.contains("MAX_ARTWORK_CANDIDATES = 32"));
        assert!(verifier.contains("lstatSync"));
        assert!(verifier.contains("isSymbolicLink"));
        assert!(verifier.contains("decodeWebpDimensions"));
        assert!(!verifier.contains("spawn("));
        assert!(!verifier.contains("exec("));

        let worker = fs::read_to_string(project.join("worker/src/main.rs")).expect("worker source");
        assert!(worker.contains("worker-echo"));
        assert!(worker.contains("stdout"));
        assert!(worker.contains("jsonrpc"));

        let protocol =
            fs::read_to_string(project.join("docs/JSONL_RPC.md")).expect("protocol docs");
        assert!(protocol.contains("jsonl-rpc-v1"));
        assert!(protocol.contains("stdout"));

        let native_worker = fs::read_to_string(project.join("docs/ENABLE_NATIVE_WORKER.md"))
            .expect("native worker guide");
        assert!(native_worker.contains("backend.binaries"));
        assert!(native_worker.contains("nativeApi"));
        assert!(native_worker.contains("timeoutMs"));
        assert!(native_worker.contains("pnpm verify"));

        assert!(result
            .next_steps
            .iter()
            .any(|step| step.contains("read-only pre-link check")));
        assert!(result
            .next_steps
            .iter()
            .any(|step| step.contains("GitHub import")));

        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn generated_verifier_accepts_bounded_png_jpeg_and_webp_artwork() {
        let (parent, project) = verifier_project("artwork-positive");

        let initial = run_verifier(&project);
        assert!(
            initial.status.success(),
            "generated PNG placeholder should verify:\n{}",
            verifier_output(&initial)
        );

        let pixels = [
            90, 110, 130, 90, 110, 130, 90, 110, 130, 90, 110, 130, 90, 110, 130, 90, 110, 130,
        ];
        for (file_name, format) in [
            ("positive.jpg", image::ImageFormat::Jpeg),
            ("positive.webp", image::ImageFormat::WebP),
        ] {
            let path = project.join("public").join(file_name);
            image::save_buffer_with_format(&path, &pixels, 2, 3, image::ColorType::Rgb8, format)
                .expect("test raster encoding");

            let mut manifest = manifest(&project);
            let declared_path = format!("public/{file_name}");
            manifest["icon"] = serde_json::json!(declared_path);
            manifest["contributes"]["commands"][0]["icon"] = serde_json::json!(declared_path);
            write_manifest(&project, &manifest);

            let output = run_verifier(&project);
            assert!(
                output.status.success(),
                "{file_name} should verify:\n{}",
                verifier_output(&output)
            );
        }

        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn generated_verifier_rejects_unsafe_artwork_paths() {
        let (parent, project) = verifier_project("artwork-paths");
        let original = manifest(&project);
        let unsafe_paths = [
            ("/absolute.png", "safe package-relative artwork path"),
            ("public/../icon.png", "empty/dot component"),
            ("public/./icon.png", "empty/dot component"),
            ("public//icon.png", "empty/dot component"),
            ("public/CON.png", "Windows device name"),
            ("public/com1.any", "Windows device name"),
            ("public/icon.", "trailing dot/space"),
            ("public/icon ", "trailing dot/space"),
            (
                "public/name:stream.png",
                "safe package-relative artwork path",
            ),
            (
                "public/\u{0000}icon.png",
                "safe package-relative artwork path",
            ),
            (
                "public/\u{0085}icon.png",
                "safe package-relative artwork path",
            ),
        ];

        for (declared_path, expected) in unsafe_paths {
            let mut changed = original.clone();
            changed["icon"] = serde_json::json!(declared_path);
            write_manifest(&project, &changed);
            assert_verifier_fails(&project, expected);
        }

        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn generated_verifier_rejects_svg_malformed_oversized_and_huge_rasters() {
        let (parent, project) = verifier_project("artwork-content");
        let original = manifest(&project);

        fs::write(
            project.join("public/vector.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
        )
        .expect("SVG fixture");
        let mut changed = original.clone();
        changed["icon"] = serde_json::json!("public/vector.svg");
        write_manifest(&project, &changed);
        assert_verifier_fails(&project, "SVG is not accepted");

        fs::write(
            project.join("public/malformed.png"),
            b"\x89PNG\r\n\x1a\nnot-a-png",
        )
        .expect("malformed PNG fixture");
        changed["icon"] = serde_json::json!("public/malformed.png");
        write_manifest(&project, &changed);
        assert_verifier_fails(&project, "malformed or incomplete PNG");

        let mut oversized = vec![0_u8; 2 * 1024 * 1024 + 1];
        oversized[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        fs::write(project.join("public/oversized.png"), oversized).expect("oversized PNG fixture");
        changed["icon"] = serde_json::json!("public/oversized.png");
        write_manifest(&project, &changed);
        assert_verifier_fails(&project, "must not exceed 2 MiB");

        let huge_pixels = vec![128_u8; 1025 * 4];
        image::save_buffer_with_format(
            project.join("public/too-wide.png"),
            &huge_pixels,
            1025,
            1,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .expect("wide PNG fixture");
        changed["icon"] = serde_json::json!("public/too-wide.png");
        write_manifest(&project, &changed);
        assert_verifier_fails(&project, "dimensions must be at most 1024×1024");

        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn generated_verifier_enforces_command_and_artwork_candidate_limits() {
        let (parent, project) = verifier_project("artwork-limits");
        let mut changed = manifest(&project);
        changed["contributes"]["commands"] = serde_json::Value::Array(
            (0..65)
                .map(|index| {
                    serde_json::json!({
                        "id": format!("command-{index}"),
                        "title": format!("Command {index}")
                    })
                })
                .collect(),
        );
        write_manifest(&project, &changed);
        assert_verifier_fails(&project, "must contain at most 64 entries");

        changed
            .as_object_mut()
            .expect("manifest object")
            .remove("icon");
        let placeholder = fs::read(project.join("public/icon.png")).expect("placeholder");
        changed["contributes"]["commands"] = serde_json::Value::Array(
            (0..33)
                .map(|index| {
                    let file_name = format!("candidate-{index}.png");
                    fs::write(project.join("public").join(&file_name), &placeholder)
                        .expect("candidate artwork");
                    serde_json::json!({
                        "id": format!("command-{index}"),
                        "title": format!("Command {index}"),
                        "icon": format!("public/{file_name}")
                    })
                })
                .collect(),
        );
        write_manifest(&project, &changed);
        assert_verifier_fails(&project, "at most 32 distinct artwork files");

        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn generated_verifier_rejects_symbolic_link_artwork_when_supported() {
        let (parent, project) = verifier_project("artwork-symlink");
        let link = project.join("public/icon-link.png");
        let target = project.join("public/icon.png");

        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&target, &link);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_file(&target, &link);

        if let Err(error) = link_result {
            eprintln!("symbolic-link test skipped because this host denied link creation: {error}");
            let _ = fs::remove_dir_all(parent);
            return;
        }

        let mut changed = manifest(&project);
        changed["icon"] = serde_json::json!("public/icon-link.png");
        write_manifest(&project, &changed);
        assert_verifier_fails(&project, "must not use a symbolic link");

        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn rejects_non_kebab_case_plugin_ids() {
        for plugin_id in [
            "IHUB-plugin-demo",
            "ihub_plugin_demo",
            "ihub--plugin",
            "-plugin",
            "123",
            "9-plugin",
        ] {
            assert!(
                validate_plugin_id(plugin_id).is_err(),
                "{plugin_id} should fail"
            );
        }
        assert!(validate_plugin_id("ihub-plugin-demo").is_ok());
    }

    #[test]
    fn never_overwrites_an_existing_project_directory() {
        let parent = temporary_parent("existing");
        let existing = parent.join("ihub-plugin-demo");
        fs::create_dir(&existing).expect("existing directory");
        fs::write(existing.join("keep.txt"), "keep").expect("existing file");

        let parent_text = parent.to_string_lossy().into_owned();
        let error = create_plugin_project(&parent_text, "ihub-plugin-demo")
            .expect_err("existing project should be rejected");
        assert!(error.contains("did not overwrite"));
        assert_eq!(
            fs::read_to_string(existing.join("keep.txt")).unwrap(),
            "keep"
        );

        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn never_overwrites_an_existing_project_file() {
        let parent = temporary_parent("existing-file");
        let existing = parent.join("ihub-plugin-demo");
        fs::write(&existing, "keep").expect("existing file");

        let parent_text = parent.to_string_lossy().into_owned();
        let error = create_plugin_project(&parent_text, "ihub-plugin-demo")
            .expect_err("existing project file should be rejected");
        assert!(error.contains("did not overwrite"));
        assert_eq!(fs::read_to_string(&existing).unwrap(), "keep");

        let _ = fs::remove_dir_all(parent);
    }
}
