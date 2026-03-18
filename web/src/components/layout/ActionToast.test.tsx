import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, test, expect, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";

const mockNavigate = vi.fn();
vi.mock("react-router", async () => {
  const actual = await vi.importActual("react-router");
  return { ...actual, useNavigate: () => mockNavigate };
});

const mockSendAction = vi.fn().mockResolvedValue(undefined);
vi.mock("../../stores/agentic-store", () => ({
  useAgenticStore: {
    getState: () => ({ sendAction: mockSendAction }),
  },
}));

vi.mock("./Toast", () => ({
  showToast: vi.fn(),
}));

import { ActionToastContainer } from "./ActionToast";
import { useNotificationStore } from "../../stores/notification-store";
import type { ActionNotification } from "../../stores/notification-store";

function makeNotification(
  overrides: Partial<ActionNotification> = {},
): ActionNotification {
  return {
    id: "loop-1",
    loopId: "loop-1",
    sessionId: "s1",
    hostId: "h1",
    hostname: "dev-server",
    toolName: "claude-code",
    status: "waiting_for_input",
    pendingToolCount: 0,
    latestToolName: null,
    createdAt: Date.now(),
    ...overrides,
  };
}

function renderWithRouter(ui: React.ReactElement) {
  return render(<MemoryRouter>{ui}</MemoryRouter>);
}

beforeEach(() => {
  vi.clearAllMocks();
  useNotificationStore.setState({
    notifications: new Map(),
    browserPermission: "default",
    browserEnabled: false,
  });
});

describe("ActionToastContainer", () => {
  test("renders nothing when no notifications", () => {
    const { container } = renderWithRouter(<ActionToastContainer />);
    expect(container.firstChild).toBeNull();
  });

  test("shows notification when present in store", () => {
    const notif = makeNotification();
    useNotificationStore.setState({
      notifications: new Map([["loop-1", notif]]),
    });
    renderWithRouter(<ActionToastContainer />);
    expect(screen.getByText("claude-code")).toBeInTheDocument();
    expect(screen.getByText("dev-server")).toBeInTheDocument();
  });

  test("shows tool count when multiple pending tools", () => {
    const notif = makeNotification({
      status: "tool_pending",
      pendingToolCount: 3,
      latestToolName: "Edit",
    });
    useNotificationStore.setState({
      notifications: new Map([["loop-1", notif]]),
    });
    renderWithRouter(<ActionToastContainer />);
    expect(screen.getByText("3 tool calls pending")).toBeInTheDocument();
  });

  test("max 3 visible with overflow indicator", () => {
    const notifications = new Map<string, ActionNotification>();
    for (let i = 0; i < 5; i++) {
      const id = `loop-${i}`;
      notifications.set(
        id,
        makeNotification({ id, loopId: id, createdAt: Date.now() + i }),
      );
    }
    useNotificationStore.setState({ notifications });
    renderWithRouter(<ActionToastContainer />);

    const alerts = screen.getAllByRole("alert");
    expect(alerts).toHaveLength(3);
    expect(screen.getByText("+2 more")).toBeInTheDocument();
  });

  test("approve button calls sendAction and dismisses", async () => {
    const notif = makeNotification();
    useNotificationStore.setState({
      notifications: new Map([["loop-1", notif]]),
    });
    renderWithRouter(<ActionToastContainer />);

    await userEvent.click(screen.getByLabelText("Approve"));
    expect(mockSendAction).toHaveBeenCalledWith("loop-1", "approve");
  });

  test("reject button calls sendAction and dismisses", async () => {
    const notif = makeNotification();
    useNotificationStore.setState({
      notifications: new Map([["loop-1", notif]]),
    });
    renderWithRouter(<ActionToastContainer />);

    await userEvent.click(screen.getByLabelText("Reject"));
    expect(mockSendAction).toHaveBeenCalledWith("loop-1", "reject");
  });

  test("dismiss button removes notification", async () => {
    const notif = makeNotification();
    useNotificationStore.setState({
      notifications: new Map([["loop-1", notif]]),
    });
    renderWithRouter(<ActionToastContainer />);

    await userEvent.click(screen.getByLabelText("Dismiss notification"));
    expect(useNotificationStore.getState().notifications.size).toBe(0);
  });

  test("go to terminal navigates and dismisses", async () => {
    const notif = makeNotification();
    useNotificationStore.setState({
      notifications: new Map([["loop-1", notif]]),
    });
    renderWithRouter(<ActionToastContainer />);

    await userEvent.click(screen.getByLabelText("Go to terminal"));
    expect(mockNavigate).toHaveBeenCalledWith("/hosts/h1/sessions/s1");
  });

  test("has role=alert on each notification", () => {
    const notif = makeNotification();
    useNotificationStore.setState({
      notifications: new Map([["loop-1", notif]]),
    });
    renderWithRouter(<ActionToastContainer />);
    expect(screen.getByRole("alert")).toBeInTheDocument();
  });
});
