import { describe, expect, it } from "vitest";

import { displayLocalPath } from "./path-display";

describe("displayLocalPath", () => {
  it("hides Windows verbatim prefixes without changing the native source value", () => {
    const source = String.raw`\\?\C:\Users\iHub\模型\assistant.vrm`;

    expect(displayLocalPath(source)).toBe(String.raw`C:\Users\iHub\模型\assistant.vrm`);
    expect(source).toBe(String.raw`\\?\C:\Users\iHub\模型\assistant.vrm`);
  });

  it("projects verbatim UNC paths and paths embedded in status text", () => {
    expect(displayLocalPath(String.raw`\\?\UNC\server\share\audio.flac`)).toBe(
      String.raw`\\server\share\audio.flac`,
    );
    expect(displayLocalPath(String.raw`无法读取 \\?\C:\Users\iHub\audio.flac`)).toBe(
      String.raw`无法读取 C:\Users\iHub\audio.flac`,
    );
  });

  it("leaves ordinary paths and URLs unchanged", () => {
    expect(displayLocalPath(String.raw`C:\Users\iHub\Documents`)).toBe(
      String.raw`C:\Users\iHub\Documents`,
    );
    expect(displayLocalPath("https://github.com/neko233-com/ihub")).toBe(
      "https://github.com/neko233-com/ihub",
    );
  });
});
