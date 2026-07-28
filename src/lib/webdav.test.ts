import { describe, expect, it } from "vitest";
import {
  formatWebDavBytes,
  isWebDavUrlWithinRoot,
  parseWebDavEndpoint,
  resolveWebDavChildUrl,
} from "./webdav";

describe("WebDAV endpoint policy", () => {
  it("normalizes a HTTPS root and keeps only same-root descendants", () => {
    const root = parseWebDavEndpoint("https://dav.example.test/remote.php/dav/files/neo");
    expect(root.href).toBe("https://dav.example.test/remote.php/dav/files/neo/");

    const child = resolveWebDavChildUrl(root, root, "folder/readme.md");
    expect(child?.href).toBe("https://dav.example.test/remote.php/dav/files/neo/folder/readme.md");
    expect(child && isWebDavUrlWithinRoot(root, child)).toBe(true);
    expect(resolveWebDavChildUrl(root, root, "https://other.example.test/secret")).toBeNull();
    expect(resolveWebDavChildUrl(root, root, "/other-root/file")).toBeNull();
  });

  it("rejects credentials, query strings, and insecure network HTTP", () => {
    expect(() => parseWebDavEndpoint("https://neo:secret@dav.example.test/root/")).toThrow("账号或密码");
    expect(() => parseWebDavEndpoint("https://dav.example.test/root/?token=secret")).toThrow("查询参数");
    expect(() => parseWebDavEndpoint("http://nas.example.test/dav/")).toThrow("HTTPS");
    expect(parseWebDavEndpoint("http://127.0.0.1:1900/dav/").href).toBe("http://127.0.0.1:1900/dav/");
  });

  it("formats file sizes without exposing raw byte counts as the primary label", () => {
    expect(formatWebDavBytes(null)).toBe("大小未知");
    expect(formatWebDavBytes(1024)).toBe("1.0 KB");
    expect(formatWebDavBytes(12 * 1024 * 1024)).toBe("12 MB");
  });
});
