import { createBrowserRouter, RouterProvider } from "react-router";
import { AppLayout } from "./components/layout/AppLayout";
import { WelcomePage } from "./pages/WelcomePage";
import { HostPage } from "./pages/HostPage";
import { SessionPage } from "./pages/SessionPage";

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
    ],
  },
]);

export default function App() {
  return <RouterProvider router={router} />;
}
