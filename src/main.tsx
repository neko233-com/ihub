import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { isDesktop } from "./lib/desktop";
import { parseApplicationRoute } from "./lib/detached-plugin-window";
import "./index.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error("iHub could not find its application root.");
}

const route = parseApplicationRoute(
  window.location.search,
  isDesktop(),
  window.location.hash,
);
async function renderApplication() {
  if (route.kind === "main") {
    return <App />;
  }
  if (route.kind === "utools-browser") {
    const { UtoolsBrowserWindowHost } = await import("./components/UtoolsBrowserWindowHost");
    return <UtoolsBrowserWindowHost route={route} />;
  }
  const {
    DetachedPluginHost,
    DetachedPluginRouteError,
  } = await import("./components/DetachedPluginHost");
  return route.kind === "detached"
    ? <DetachedPluginHost route={route} />
    : <DetachedPluginRouteError message={route.message} />;
}

void renderApplication().then((application) => {
  createRoot(root).render(
    <StrictMode>
      {application}
    </StrictMode>,
  );
});
