export const PLUGIN_HOST_EVENT_QUEUE_LIMIT = 64;
export const PLUGIN_PENDING_EVENT_ID_LIMIT = 256;

export function enqueueBoundedPluginHostEvent<T>(
  queue: T[],
  event: T,
  limit = PLUGIN_HOST_EVENT_QUEUE_LIMIT,
): void {
  const boundedLimit = Math.max(1, Math.floor(limit));
  while (queue.length >= boundedLimit) {
    queue.shift();
  }
  queue.push(event);
}

export function restoreFailedPluginHostEventTail<T>(
  queue: T[],
  events: readonly T[],
  failedIndex: number,
  limit = PLUGIN_HOST_EVENT_QUEUE_LIMIT,
): void {
  const boundedLimit = Math.max(1, Math.floor(limit));
  const safeFailedIndex = Math.max(0, Math.min(events.length, Math.floor(failedIndex)));
  const combined = [...events.slice(safeFailedIndex), ...queue];
  const bounded = combined.slice(Math.max(0, combined.length - boundedLimit));
  queue.splice(0, queue.length, ...bounded);
}

export function rememberBoundedPluginEventId(
  ids: Set<string>,
  id: string,
  limit = PLUGIN_PENDING_EVENT_ID_LIMIT,
): void {
  const boundedLimit = Math.max(1, Math.floor(limit));
  while (ids.size >= boundedLimit) {
    const oldest = ids.values().next().value;
    if (typeof oldest !== "string") {
      break;
    }
    ids.delete(oldest);
  }
  ids.add(id);
}
