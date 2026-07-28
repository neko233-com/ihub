/**
 * The Cloud Drive workbench owns one interaction model. Providers describe
 * capability and sign-in policy here; they do not bring their own embedded
 * website or layout into the launcher.
 */
export type CloudDriveCapability = "browse" | "download" | "upload" | "move" | "delete" | "background-sync";

export type CloudDriveProviderId = "webdav" | "aliyun-drive" | "baidu-netdisk" | "onedrive";

export type CloudDriveProviderStatus = "available" | "planned";

export interface CloudDriveProvider {
  capabilities: readonly CloudDriveCapability[];
  description: string;
  id: CloudDriveProviderId;
  name: string;
  /** A provider-specific native adapter owns this flow; no web login is embedded. */
  signIn: "webdav" | "oauth-pkce";
  status: CloudDriveProviderStatus;
  supportsPrivateDeployment: boolean;
}

export const cloudDriveProviders: readonly CloudDriveProvider[] = [
  {
    id: "webdav",
    name: "WebDAV / 私有部署",
    description: "Nextcloud、群晖、坚果云或自建 WebDAV：同一目录浏览与下载工作面。",
    signIn: "webdav",
    status: "available",
    supportsPrivateDeployment: true,
    capabilities: ["browse", "download", "upload"],
  },
  {
    id: "aliyun-drive",
    name: "阿里云盘",
    description: "将使用已注册 OAuth / PKCE 原生适配器接入同一文件操作界面。",
    signIn: "oauth-pkce",
    status: "planned",
    supportsPrivateDeployment: false,
    capabilities: ["browse", "download", "upload"],
  },
  {
    id: "baidu-netdisk",
    name: "百度网盘",
    description: "将使用已注册 OAuth / PKCE 原生适配器接入同一文件操作界面。",
    signIn: "oauth-pkce",
    status: "planned",
    supportsPrivateDeployment: false,
    capabilities: ["browse", "download", "upload"],
  },
  {
    id: "onedrive",
    name: "OneDrive",
    description: "将使用已注册 OAuth / PKCE 原生适配器接入同一文件操作界面。",
    signIn: "oauth-pkce",
    status: "planned",
    supportsPrivateDeployment: false,
    capabilities: ["browse", "download", "upload"],
  },
];

export function cloudDriveProvider(id: CloudDriveProviderId) {
  return cloudDriveProviders.find((provider) => provider.id === id);
}
