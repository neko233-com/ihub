# Cloud Drive boundary

Cloud Drive is a first-party Tauri subsystem, not a general plugin iframe.

## One UI, provider adapters underneath

The launcher owns a single connection and file-operation workbench. A provider contributes only its native adapter, sign-in policy and capability set; it does not inject a provider-specific web UI. This keeps WebDAV private deployments, NAS services and future Aliyun/Baidu/OneDrive connections on the same browse/download/upload interaction model.

## Current WebDAV slice

- A person explicitly types a WebDAV root and presses **连接并浏览**.
- Rust performs one `PROPFIND Depth: 1` request with a 20-second deadline and an 8 MiB XML ceiling.
- The password crosses trusted built-in IPC only for that first connection. After authentication, Rust returns a random UUIDv4 `connectionId`; directory, download, upload and disconnect requests contain no endpoint, account or password.
- The renderer clears its password state after every connection attempt. A live native session keeps a zeroizing password allocation for at most 30 minutes idle or eight hours total; at most four sessions exist per process.
- **记住到系统凭据库** is opt-in. Windows stores the bounded secret in Credential Manager and macOS stores it in Keychain. `cloud-profiles-v1.json` contains only display metadata (provider, label, endpoint, account and timestamps), never a password or token. Listing profiles does not read the vault.
- Disconnecting removes only the in-memory session. **忘记** first revokes that profile's sessions and then deletes its system credential and metadata.
- Only HTTPS is accepted for network endpoints. HTTP is limited to `localhost`, `127.0.0.1`, or `::1` for development.
- URLs with userinfo, query parameters or fragments are rejected. Redirects are rejected, and renderer XML parsing discards a response entry outside the configured origin/root.
- A selected remote file can be downloaded only after a native Save As dialog. Bytes stream to a same-directory `.part` file and are published with a no-clobber hard-link step; no file bytes cross Tauri IPC or renderer memory.
- A selected local file can be uploaded only through a native picker. Rust streams it to a UUID-named remote staging object, then sends WebDAV `MOVE` with `Overwrite: F`; an existing destination is never replaced. No local path crosses Tauri IPC.
- This slice still excludes background sync, mount, indexing, arbitrary automatic upload, remote move/delete UI actions, and OAuth token persistence.

## Provider adapters

Aliyun Drive, Baidu Netdisk, OneDrive, and similar providers require separate native adapters. Each adapter must fix its own authorize/token/API hosts, OAuth scopes, PKCE/state validation, and registered redirect URI in source code. The renderer must never supply an arbitrary OAuth URL or receive access/refresh tokens.

The credential envelope already distinguishes provider and secret kind so future reviewed OAuth adapters can store a refresh token without treating it as a WebDAV password. PKCE verifiers, state, access tokens and provider hosts still belong entirely to each native adapter. File transfers remain streamed to a user-chosen destination or from a native picker, with cancellation and no whole-file renderer buffering.

## Threat boundary

iHub intentionally permits unsandboxed native plugins and arbitrary binaries. The OS vault prevents accidental plaintext persistence and removes long-lived secrets from renderer IPC; it cannot isolate credentials from a malicious program already running as the same OS user. Cloud Drive commands therefore remain first-party-only and are never exposed through the plugin bridge.
