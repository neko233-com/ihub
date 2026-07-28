import { describe, expect, it } from "vitest";
import {
  buildCloudDriveDisconnectRequest,
  buildCloudProfileForgetRequest,
  buildWebDavConnectRequest,
  buildWebDavDownloadRequest,
  buildWebDavListRequest,
  buildWebDavSavedConnectRequest,
  buildWebDavUploadRequest,
} from "./cloud-drive-session";

const connectionId = "2ac314b4-2427-46eb-aa11-c2ba0b1bf84d";
const profileId = "4978e976-128c-462b-8296-ee82a160b48f";

function expectNoCredentialFields(value: unknown) {
  const json = JSON.stringify(value);
  expect(json).not.toContain("endpoint");
  expect(json).not.toContain("username");
  expect(json).not.toContain("password");
}

describe("Cloud Drive session request builders", () => {
  it("keeps credentials only in the one-time WebDAV connect request", () => {
    const request = buildWebDavConnectRequest({
      endpoint: "https://dav.example.test/root/",
      username: "neko",
      password: "one-time-secret",
      remember: true,
      label: "家庭 NAS",
    });

    expect(request).toEqual({
      endpoint: "https://dav.example.test/root/",
      username: "neko",
      password: "one-time-secret",
      remember: true,
      label: "家庭 NAS",
    });
  });

  it("connects a saved profile with only its opaque profile id", () => {
    const taintedRendererState = {
      profileId,
      endpoint: "https://should-not-leak.example/",
      username: "should-not-leak",
      password: "should-not-leak",
    };

    const request = buildWebDavSavedConnectRequest(taintedRendererState);

    expect(request).toEqual({ profileId });
    expectNoCredentialFields(request);
  });

  it("projects list, download, upload, and disconnect payloads from connection ids", () => {
    const taintedRendererState = {
      connectionId,
      directory: "https://dav.example.test/root/photos/",
      remoteUrl: "https://dav.example.test/root/photos/cat.png",
      suggestedFilename: "cat.png",
      endpoint: "https://should-not-leak.example/",
      username: "should-not-leak",
      password: "should-not-leak",
    };

    const requests = [
      buildWebDavListRequest(taintedRendererState),
      buildWebDavDownloadRequest(taintedRendererState),
      buildWebDavUploadRequest(taintedRendererState),
      buildCloudDriveDisconnectRequest(taintedRendererState),
    ];

    expect(requests).toEqual([
      {
        connectionId,
        directory: "https://dav.example.test/root/photos/",
      },
      {
        connectionId,
        remoteUrl: "https://dav.example.test/root/photos/cat.png",
        suggestedFilename: "cat.png",
      },
      {
        connectionId,
        directory: "https://dav.example.test/root/photos/",
      },
      { connectionId },
    ]);
    for (const request of requests) {
      expectNoCredentialFields(request);
    }
  });

  it("omits an absent optional directory instead of serializing undefined", () => {
    expect(buildWebDavListRequest({ connectionId })).toEqual({ connectionId });
    expect(JSON.stringify(buildWebDavListRequest({ connectionId }))).toBe(
      `{"connectionId":"${connectionId}"}`,
    );
  });

  it("forgets a saved profile with only its opaque profile id", () => {
    const request = buildCloudProfileForgetRequest({
      profileId,
      password: "should-not-leak",
    } as { profileId: string } & { password: string });

    expect(request).toEqual({ profileId });
    expectNoCredentialFields(request);
  });

  it.each([
    () => buildWebDavSavedConnectRequest({ profileId: "  " }),
    () => buildWebDavListRequest({ connectionId: "" }),
    () => buildWebDavDownloadRequest({
      connectionId: "\n",
      remoteUrl: "https://dav.example.test/file",
      suggestedFilename: "file",
    }),
    () => buildWebDavUploadRequest({ connectionId: "\t", directory: "/" }),
    () => buildCloudDriveDisconnectRequest({ connectionId: " " }),
    () => buildCloudProfileForgetRequest({ profileId: "" }),
  ])("rejects an empty connection or profile id before native invocation", (build) => {
    expect(build).toThrow(/(?:connectionId|profileId) 不能为空/);
  });
});
