import { describe, expect, it } from "vitest";
import { cloudDriveProvider, cloudDriveProviders } from "./cloud-drive-providers";

describe("uniform Cloud Drive provider catalog", () => {
  it("keeps private WebDAV as the only currently actionable adapter", () => {
    const available = cloudDriveProviders.filter((provider) => provider.status === "available");
    expect(available.map((provider) => provider.id)).toEqual(["webdav"]);
    expect(cloudDriveProvider("webdav")).toMatchObject({
      signIn: "webdav",
      supportsPrivateDeployment: true,
      capabilities: ["browse", "download", "upload"],
    });
  });

  it("requires native OAuth adapters instead of embedded provider sites", () => {
    const planned = cloudDriveProviders.filter((provider) => provider.status === "planned");
    expect(planned).toHaveLength(3);
    expect(planned.every((provider) => provider.signIn === "oauth-pkce")).toBe(true);
  });
});
