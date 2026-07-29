import { describe, expect, it } from "vitest";
import {
  completePluginSearchProviderReadiness,
  createPluginSearchProviderReadinessBootstrap,
  transitionPluginSearchProviderReadiness,
  type PluginSearchProviderReadinessEvent,
} from "./plugin-search-provider-readiness";

function applyReadinessEvent(
  providers: Set<string>,
  event: PluginSearchProviderReadinessEvent,
) {
  const prefix = `${event.pluginId}:`;
  if (event.providerId === undefined) {
    for (const key of providers) {
      if (key.startsWith(prefix)) {
        providers.delete(key);
      }
    }
    return;
  }
  const key = `${prefix}${event.providerId}`;
  if (event.registered) {
    providers.add(key);
  } else {
    providers.delete(key);
  }
}

describe("plugin search provider readiness bootstrap", () => {
  it("replays a registration that arrives after an older snapshot was issued", () => {
    let bootstrap = createPluginSearchProviderReadinessBootstrap();
    const transition = transitionPluginSearchProviderReadiness(bootstrap, {
      pluginId: "com.example.search",
      providerId: "docs",
      registered: true,
    });
    bootstrap = transition.bootstrap;
    expect(transition.eventToApply).toBeNull();

    const completion = completePluginSearchProviderReadiness(bootstrap, []);
    const providers = new Set<string>();
    completion.eventsToReplay.forEach((event) => applyReadinessEvent(providers, event));

    expect([...providers]).toEqual(["com.example.search:docs"]);
    expect(completion.bootstrap.live).toBe(true);
  });

  it("replays an unregister that arrives after a snapshot containing the provider", () => {
    let bootstrap = createPluginSearchProviderReadinessBootstrap();
    bootstrap = transitionPluginSearchProviderReadiness(bootstrap, {
      pluginId: "com.example.search",
      providerId: "docs",
      registered: false,
    }).bootstrap;

    const completion = completePluginSearchProviderReadiness(
      bootstrap,
      ["com.example.search:docs"],
    );
    const providers = new Set(completion.snapshot);
    completion.eventsToReplay.forEach((event) => applyReadinessEvent(providers, event));

    expect([...providers]).toEqual([]);
  });

  it("compacts buffered changes without changing clear-then-register ordering", () => {
    let bootstrap = createPluginSearchProviderReadinessBootstrap();
    for (const event of [
      {
        pluginId: "com.example.search",
        providerId: "docs",
        registered: true,
      },
      {
        pluginId: "com.example.search",
        registered: false,
      },
      {
        pluginId: "com.example.search",
        providerId: "docs",
        registered: true,
      },
    ] satisfies PluginSearchProviderReadinessEvent[]) {
      bootstrap = transitionPluginSearchProviderReadiness(bootstrap, event).bootstrap;
    }

    const completion = completePluginSearchProviderReadiness(
      bootstrap,
      ["com.example.search:legacy"],
    );
    const providers = new Set(completion.snapshot);
    completion.eventsToReplay.forEach((event) => applyReadinessEvent(providers, event));

    expect(completion.eventsToReplay).toEqual([
      {
        pluginId: "com.example.search",
        registered: false,
      },
      {
        pluginId: "com.example.search",
        providerId: "docs",
        registered: true,
      },
    ]);
    expect([...providers]).toEqual(["com.example.search:docs"]);
  });

  it("passes later live events through instead of retaining them", () => {
    const completed = completePluginSearchProviderReadiness(
      createPluginSearchProviderReadinessBootstrap(),
      [],
    );
    const event = {
      pluginId: "com.example.search",
      providerId: "docs",
      registered: false,
    };

    const transition = transitionPluginSearchProviderReadiness(
      completed.bootstrap,
      event,
    );

    expect(transition.eventToApply).toEqual(event);
    expect(transition.bootstrap.pendingEvents).toEqual([]);
  });
});
