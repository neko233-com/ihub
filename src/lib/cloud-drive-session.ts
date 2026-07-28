export interface CloudProfileView {
  id: string;
  provider: string;
  label: string;
  endpoint: string;
  username: string;
  createdAt: string;
  updatedAt: string;
}

export interface WebDavConnectResult {
  connectionId: string;
  profile: CloudProfileView | null;
  endpoint: string;
  directory: string;
  xml: string;
}

/** Credentials are permitted only in the one-time, unsaved connection request. */
export interface WebDavConnectRequest {
  endpoint: string;
  username: string;
  password: string;
  remember: boolean;
  label?: string;
}

export interface WebDavSavedConnectRequest {
  profileId: string;
}

export interface WebDavListRequest {
  connectionId: string;
  directory?: string;
}

export interface WebDavDownloadRequest {
  connectionId: string;
  remoteUrl: string;
  suggestedFilename: string;
}

export interface WebDavUploadRequest {
  connectionId: string;
  directory: string;
}

export interface CloudDriveDisconnectRequest {
  connectionId: string;
}

export interface CloudProfileForgetRequest {
  profileId: string;
}

function requireOpaqueId(value: string, field: "connectionId" | "profileId") {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(`${field} 不能为空。`);
  }
  return value;
}

/**
 * Projects only the credential-bearing fields accepted by the initial native
 * connection command. Callers must discard their password after it succeeds.
 */
export function buildWebDavConnectRequest(
  request: WebDavConnectRequest,
): WebDavConnectRequest {
  const projected: WebDavConnectRequest = {
    endpoint: request.endpoint,
    username: request.username,
    password: request.password,
    remember: request.remember,
  };
  if (request.label !== undefined) {
    projected.label = request.label;
  }
  return projected;
}

export function buildWebDavSavedConnectRequest(
  request: WebDavSavedConnectRequest,
): WebDavSavedConnectRequest {
  return {
    profileId: requireOpaqueId(request.profileId, "profileId"),
  };
}

/**
 * Every operation after connect is deliberately projected from an opaque
 * connection id. Extra renderer state (including credentials) cannot leak into
 * the Tauri payload through object spreading.
 */
export function buildWebDavListRequest(
  request: WebDavListRequest,
): WebDavListRequest {
  const projected: WebDavListRequest = {
    connectionId: requireOpaqueId(request.connectionId, "connectionId"),
  };
  if (request.directory !== undefined) {
    projected.directory = request.directory;
  }
  return projected;
}

export function buildWebDavDownloadRequest(
  request: WebDavDownloadRequest,
): WebDavDownloadRequest {
  return {
    connectionId: requireOpaqueId(request.connectionId, "connectionId"),
    remoteUrl: request.remoteUrl,
    suggestedFilename: request.suggestedFilename,
  };
}

export function buildWebDavUploadRequest(
  request: WebDavUploadRequest,
): WebDavUploadRequest {
  return {
    connectionId: requireOpaqueId(request.connectionId, "connectionId"),
    directory: request.directory,
  };
}

export function buildCloudDriveDisconnectRequest(
  request: CloudDriveDisconnectRequest,
): CloudDriveDisconnectRequest {
  return {
    connectionId: requireOpaqueId(request.connectionId, "connectionId"),
  };
}

export function buildCloudProfileForgetRequest(
  request: CloudProfileForgetRequest,
): CloudProfileForgetRequest {
  return {
    profileId: requireOpaqueId(request.profileId, "profileId"),
  };
}
