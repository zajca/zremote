import { Outlet } from "react-router";
import { Sidebar } from "./Sidebar";
import { CommandPalette } from "./CommandPalette";
import { ToastContainer } from "./Toast";
import { ReconnectBanner } from "./ReconnectBanner";
import { ErrorBoundary } from "./ErrorBoundary";

export function AppLayout() {
  return (
    <div className="flex h-screen overflow-hidden">
      <ReconnectBanner />
      <Sidebar />
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
