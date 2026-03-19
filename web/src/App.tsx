import { lazy, Suspense } from "react";
import { createBrowserRouter, RouterProvider } from "react-router";
import { AppLayout } from "./components/layout/AppLayout";
import { ModeProvider } from "./hooks/useMode";
import { WelcomePage } from "./pages/WelcomePage";
import { HostPage } from "./pages/HostPage";
import { SessionPage } from "./pages/SessionPage";
import { ProjectPage } from "./pages/ProjectPage";
import { SettingsPage } from "./components/settings/SettingsPage";

const AnalyticsDashboard = lazy(() =>
  import("./pages/AnalyticsDashboard").then((m) => ({
    default: m.AnalyticsDashboard,
  })),
);

const HistoryBrowser = lazy(() =>
  import("./pages/HistoryBrowser").then((m) => ({
    default: m.HistoryBrowser,
  })),
);

function LazyFallback() {
  return (
    <div className="flex h-full items-center justify-center text-sm text-text-tertiary">
      Loading...
    </div>
  );
}

const router = createBrowserRouter([
  {
    element: <AppLayout />,
    children: [
      { index: true, element: <WelcomePage /> },
      { path: "hosts/:hostId", element: <HostPage /> },
      {
        path: "hosts/:hostId/sessions/:sessionId",
        element: <SessionPage />,
      },
      {
        path: "analytics",
        element: (
          <Suspense fallback={<LazyFallback />}>
            <AnalyticsDashboard />
          </Suspense>
        ),
      },
      {
        path: "history",
        element: (
          <Suspense fallback={<LazyFallback />}>
            <HistoryBrowser />
          </Suspense>
        ),
      },
      { path: "projects/:projectId", element: <ProjectPage /> },
      { path: "settings", element: <SettingsPage /> },
    ],
  },
]);

export default function App() {
  return (
    <ModeProvider>
      <RouterProvider router={router} />
    </ModeProvider>
  );
}
