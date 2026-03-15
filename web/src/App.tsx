import { createBrowserRouter, RouterProvider } from "react-router";
import { AppLayout } from "./components/layout/AppLayout";
import { WelcomePage } from "./pages/WelcomePage";
import { HostPage } from "./pages/HostPage";
import { SessionPage } from "./pages/SessionPage";
import { AgenticLoopPage } from "./pages/AgenticLoopPage";

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
        path: "hosts/:hostId/sessions/:sessionId/loops/:loopId",
        element: <AgenticLoopPage />,
      },
    ],
  },
]);

export default function App() {
  return <RouterProvider router={router} />;
}
