import { useCallback, useEffect, useRef, useState } from "react";
import { Outlet } from "react-router";
import { Sidebar } from "./Sidebar";
import { CommandPalette } from "./CommandPalette";
import { ToastContainer } from "./Toast";
import { ReconnectBanner } from "./ReconnectBanner";
import { ErrorBoundary } from "./ErrorBoundary";

const SIDEBAR_KEY = "zremote:sidebar-visible";

type SidebarMode = "pinned" | "unpinned";

function getPersistedMode(): SidebarMode {
  try {
    const val = localStorage.getItem(SIDEBAR_KEY);
    if (val === "unpinned" || val === "false") return "unpinned";
  } catch {
    // ignore
  }
  return "pinned";
}

function persistMode(mode: SidebarMode) {
  try {
    localStorage.setItem(SIDEBAR_KEY, mode);
  } catch {
    // ignore
  }
}

export function AppLayout() {
  const [sidebarMode, setSidebarMode] = useState<SidebarMode>(getPersistedMode);
  const [hoverVisible, setHoverVisible] = useState(false);
  const hideTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearHideTimeout = useCallback(() => {
    if (hideTimeoutRef.current) {
      clearTimeout(hideTimeoutRef.current);
      hideTimeoutRef.current = null;
    }
  }, []);

  const pin = useCallback(() => {
    clearHideTimeout();
    setHoverVisible(false);
    setSidebarMode("pinned");
    persistMode("pinned");
  }, [clearHideTimeout]);

  const unpin = useCallback(() => {
    setSidebarMode("unpinned");
    persistMode("unpinned");
  }, []);

  const toggleSidebar = useCallback(() => {
    setSidebarMode((prev) => {
      const next = prev === "pinned" ? "unpinned" : "pinned";
      persistMode(next);
      if (next === "pinned") {
        clearHideTimeout();
        setHoverVisible(false);
      }
      return next;
    });
  }, [clearHideTimeout]);

  const handleTriggerEnter = useCallback(() => {
    clearHideTimeout();
    setHoverVisible(true);
  }, [clearHideTimeout]);

  const handleSidebarEnter = useCallback(() => {
    clearHideTimeout();
  }, [clearHideTimeout]);

  const handleSidebarLeave = useCallback(() => {
    clearHideTimeout();
    hideTimeoutRef.current = setTimeout(() => {
      setHoverVisible(false);
    }, 200);
  }, [clearHideTimeout]);

  useEffect(() => {
    return () => clearHideTimeout();
  }, [clearHideTimeout]);

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

  const isPinned = sidebarMode === "pinned";

  return (
    <div className="flex h-screen overflow-hidden">
      <ReconnectBanner />

      {isPinned && <Sidebar pinned onPin={pin} onUnpin={unpin} />}

      {!isPinned && (
        <>
          {/* Invisible trigger zone on the left edge */}
          <div
            className="fixed left-0 top-0 z-40 h-full w-2"
            onMouseEnter={handleTriggerEnter}
          />

          {/* Overlay sidebar */}
          <div
            className={`fixed left-0 top-0 z-50 h-full transition-transform duration-200 ${
              hoverVisible
                ? "translate-x-0"
                : "-translate-x-full"
            }`}
            onMouseEnter={handleSidebarEnter}
            onMouseLeave={handleSidebarLeave}
          >
            <div className="h-full shadow-[4px_0_12px_rgba(0,0,0,0.3)]">
              <Sidebar pinned={false} onPin={pin} onUnpin={unpin} />
            </div>
          </div>
        </>
      )}

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
