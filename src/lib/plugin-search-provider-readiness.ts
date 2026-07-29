export const PLUGIN_SEARCH_PROVIDER_READINESS_BUFFER_LIMIT = 4_096;

export interface PluginSearchProviderReadinessEvent {
  pluginId: string;
  providerId?: string;
  registered: boolean;
}

export interface PluginSearchProviderReadinessBootstrap {
  live: boolean;
  pendingEvents: readonly PluginSearchProviderReadinessEvent[];
}

export interface PluginSearchProviderReadinessTransition {
  bootstrap: PluginSearchProviderReadinessBootstrap;
  eventToApply: PluginSearchProviderReadinessEvent | null;
}

export interface PluginSearchProviderReadinessCompletion<TSnapshot> {
  bootstrap: PluginSearchProviderReadinessBootstrap;
  eventsToReplay: readonly PluginSearchProviderReadinessEvent[];
  snapshot: TSnapshot;
}

export function createPluginSearchProviderReadinessBootstrap(): PluginSearchProviderReadinessBootstrap {
  return {
    live: false,
    pendingEvents: [],
  };
}

function retainLatestReadinessEvent(
  pendingEvents: readonly PluginSearchProviderReadinessEvent[],
  event: PluginSearchProviderReadinessEvent,
): readonly PluginSearchProviderReadinessEvent[] {
  const next = pendingEvents.filter((pending) => {
    if (pending.pluginId !== event.pluginId) {
      return true;
    }
    if (event.providerId === undefined) {
      // A plugin-level dispose supersedes every earlier provider mutation for
      // that plugin. Later provider registrations remain after this clear.
      return false;
    }
    return pending.providerId !== event.providerId;
  });
  next.push(event);
  return next.slice(-PLUGIN_SEARCH_PROVIDER_READINESS_BUFFER_LIMIT);
}

/**
 * Buffers readiness changes until the host's initial native snapshot arrives.
 * Once live, events pass through immediately. The transition is pure so the
 * startup ordering can be exercised without React or Tauri.
 */
export function transitionPluginSearchProviderReadiness(
  bootstrap: PluginSearchProviderReadinessBootstrap,
  event: PluginSearchProviderReadinessEvent,
): PluginSearchProviderReadinessTransition {
  if (bootstrap.live) {
    return {
      bootstrap,
      eventToApply: event,
    };
  }
  return {
    bootstrap: {
      live: false,
      pendingEvents: retainLatestReadinessEvent(bootstrap.pendingEvents, event),
    },
    eventToApply: null,
  };
}

/**
 * Applies one authoritative snapshot before replaying every later native
 * mutation in arrival order. The caller completes each bootstrap exactly once.
 */
export function completePluginSearchProviderReadiness<TSnapshot>(
  bootstrap: PluginSearchProviderReadinessBootstrap,
  snapshot: TSnapshot,
): PluginSearchProviderReadinessCompletion<TSnapshot> {
  return {
    bootstrap: {
      live: true,
      pendingEvents: [],
    },
    eventsToReplay: bootstrap.live ? [] : bootstrap.pendingEvents,
    snapshot,
  };
}
