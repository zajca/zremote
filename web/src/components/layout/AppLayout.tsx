import { useCallback, useEffect, useState } from "react";
import { Outlet } from "react-router";
import { Sidebar } from "./Sidebar";
import { CommandPalette } from "./CommandPalette";
import { ToastContainer } from "./Toast";
import { ReconnectBanner } from "./ReconnectBanner";
import { ErrorBoundary } from "./ErrorBoundary";

const SIDEBAR_KEY = "myremote:sidebar-visible";

function getPersistedSidebar(): boolean {
  try {
    const val = localStorage.getItem(SIDEBAR_KEY);
    if (val === "false") return false;
  } catch {
    // ignore
  }
  return true;
}

export function AppLayout() {
  const [sidebarVisible, setSidebarVisible] = useState(getPersistedSidebar);

  const toggleSidebar = useCallback(() => {
    setSidebarVisible((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(SIDEBAR_KEY, String(next));
      } catch {
        // ignore
      }
      return next;
    });
  }, []);

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if ((e.ctrlKey || e.metaKey) && e.key === "b") {
        e.preventDefault();
        toggleSidebar();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [toggleSidebar]);

  return (
    <div className="flex h-screen overflow-hidden">
      <ReconnectBanner />
      {sidebarVisible && <Sidebar onCollapse={toggleSidebar} />}
      <main className="flex-1 overflow-auto bg-bg-primary">
        <ErrorBoundary>
          <Outlet />
        </ErrorBoundary>
      </main>
      <CommandPalette />
      <ToastContainer />
    </div>
  );
}
