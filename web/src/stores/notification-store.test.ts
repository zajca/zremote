import { describe, test, expect, beforeEach, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";

vi.mock("../lib/browser-notifications", () => ({
  getBrowserPermission: vi.fn(() => "default" as NotificationPermission),
}));

import { useNotificationStore } from "./notification-store";
import type { ActionNotification } from "./notification-store";

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
    argumentsPreview: null,
    createdAt: Date.now(),
    sessionName: null,
    projectName: null,
    taskName: null,
    ...overrides,
  };
}

beforeEach(() => {
  vi.restoreAllMocks();
  useNotificationStore.setState({
    notifications: new Map(),
    browserPermission: "default",
    browserEnabled: false,
  });
});

describe("addOrUpdate", () => {
  test("creates new notification", () => {
    const { result } = renderHook(() => useNotificationStore());
    const notif = makeNotification();
    act(() => result.current.addOrUpdate(notif));
    expect(result.current.notifications.size).toBe(1);
    expect(result.current.notifications.get("loop-1")).toEqual(notif);
  });

  test("updates existing notification (replaces for non-tool_pending)", () => {
    const { result } = renderHook(() => useNotificationStore());
    const notif = makeNotification();
    act(() => result.current.addOrUpdate(notif));

    const updated = makeNotification({ toolName: "updated-tool" });
    act(() => result.current.addOrUpdate(updated));
    expect(result.current.notifications.size).toBe(1);
    expect(result.current.notifications.get("loop-1")?.toolName).toBe(
      "updated-tool",
    );
  });

  test("increments pendingToolCount for tool_pending on existing", () => {
    const { result } = renderHook(() => useNotificationStore());
    const notif = makeNotification({
      status: "tool_pending",
      pendingToolCount: 1,
      latestToolName: "Read",
    });
    act(() => result.current.addOrUpdate(notif));

    const update = makeNotification({
      status: "tool_pending",
      pendingToolCount: 1,
      latestToolName: "Edit",
    });
    act(() => result.current.addOrUpdate(update));

    const stored = result.current.notifications.get("loop-1")!;
    expect(stored.pendingToolCount).toBe(2);
    expect(stored.latestToolName).toBe("Edit");
  });

  test("carries argumentsPreview on tool_pending update", () => {
    const { result } = renderHook(() => useNotificationStore());
    const notif = makeNotification({
      status: "tool_pending",
      pendingToolCount: 1,
      latestToolName: "Read",
      argumentsPreview: "/foo.ts",
    });
    act(() => result.current.addOrUpdate(notif));

    const update = makeNotification({
      status: "tool_pending",
      pendingToolCount: 1,
      latestToolName: "Edit",
      argumentsPreview: "/bar.ts",
    });
    act(() => result.current.addOrUpdate(update));

    const stored = result.current.notifications.get("loop-1")!;
    expect(stored.argumentsPreview).toBe("/bar.ts");
  });

  test("deduplicates by loopId", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification()));
    act(() => result.current.addOrUpdate(makeNotification()));
    expect(result.current.notifications.size).toBe(1);
  });
});

describe("dismiss", () => {
  test("removes notification by loopId", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification()));
    act(() => result.current.dismiss("loop-1"));
    expect(result.current.notifications.size).toBe(0);
  });

  test("no-op for non-existent loopId", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.dismiss("nonexistent"));
    expect(result.current.notifications.size).toBe(0);
  });
});

describe("dismissAll", () => {
  test("clears all notifications", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification({ id: "l1", loopId: "l1" })));
    act(() => result.current.addOrUpdate(makeNotification({ id: "l2", loopId: "l2" })));
    expect(result.current.notifications.size).toBe(2);

    act(() => result.current.dismissAll());
    expect(result.current.notifications.size).toBe(0);
  });
});

describe("handleLoopResolved", () => {
  test("removes notification for that loopId", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification()));
    act(() => result.current.handleLoopResolved("loop-1"));
    expect(result.current.notifications.size).toBe(0);
  });

  test("no-op for non-existent loopId", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.handleLoopResolved("nonexistent"));
    expect(result.current.notifications.size).toBe(0);
  });
});

describe("handleToolResolved", () => {
  test("decrements pendingToolCount", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() =>
      result.current.addOrUpdate(
        makeNotification({
          status: "tool_pending",
          pendingToolCount: 3,
        }),
      ),
    );
    act(() => result.current.handleToolResolved("loop-1"));
    expect(result.current.notifications.get("loop-1")?.pendingToolCount).toBe(
      2,
    );
  });

  test("removes notification when count reaches 0 and status is tool_pending", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() =>
      result.current.addOrUpdate(
        makeNotification({
          status: "tool_pending",
          pendingToolCount: 1,
        }),
      ),
    );
    act(() => result.current.handleToolResolved("loop-1"));
    expect(result.current.notifications.size).toBe(0);
  });

  test("does not remove if status is not tool_pending", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() =>
      result.current.addOrUpdate(
        makeNotification({
          status: "waiting_for_input",
          pendingToolCount: 1,
        }),
      ),
    );
    act(() => result.current.handleToolResolved("loop-1"));
    expect(result.current.notifications.size).toBe(1);
    expect(result.current.notifications.get("loop-1")?.pendingToolCount).toBe(
      0,
    );
  });

  test("no-op for non-existent loopId", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.handleToolResolved("nonexistent"));
    expect(result.current.notifications.size).toBe(0);
  });
});

describe("setBrowserEnabled", () => {
  test("sets browserEnabled", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.setBrowserEnabled(true));
    expect(result.current.browserEnabled).toBe(true);
    act(() => result.current.setBrowserEnabled(false));
    expect(result.current.browserEnabled).toBe(false);
  });
});

describe("setBrowserPermission", () => {
  test("sets browserPermission", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.setBrowserPermission("granted"));
    expect(result.current.browserPermission).toBe("granted");
    act(() => result.current.setBrowserPermission("unsupported"));
    expect(result.current.browserPermission).toBe("unsupported");
  });
});

describe("patchContext", () => {
  test("patches sessionName on existing notification", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification()));
    act(() => result.current.patchContext("loop-1", { sessionName: "my-session" }));
    expect(result.current.notifications.get("loop-1")?.sessionName).toBe("my-session");
  });

  test("patches projectName on existing notification", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification()));
    act(() => result.current.patchContext("loop-1", { projectName: "my-project" }));
    expect(result.current.notifications.get("loop-1")?.projectName).toBe("my-project");
  });

  test("patches taskName on existing notification", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification()));
    act(() => result.current.patchContext("loop-1", { taskName: "implement feature" }));
    expect(result.current.notifications.get("loop-1")?.taskName).toBe("implement feature");
  });

  test("no-op for non-existent loopId", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.patchContext("nonexistent", { sessionName: "x" }));
    expect(result.current.notifications.size).toBe(0);
  });

  test("does not overwrite other fields", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification({ toolName: "claude-code" })));
    act(() => result.current.patchContext("loop-1", { sessionName: "s" }));
    expect(result.current.notifications.get("loop-1")?.toolName).toBe("claude-code");
  });
});

describe("addOrUpdate context backfill", () => {
  test("backfills null sessionName from incoming on tool_pending", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification({
      status: "tool_pending",
      pendingToolCount: 1,
      sessionName: null,
    })));
    act(() => result.current.addOrUpdate(makeNotification({
      status: "tool_pending",
      pendingToolCount: 1,
      sessionName: "resolved-session",
    })));
    expect(result.current.notifications.get("loop-1")?.sessionName).toBe("resolved-session");
  });

  test("preserves existing sessionName over incoming null on tool_pending", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification({
      status: "tool_pending",
      pendingToolCount: 1,
      sessionName: "existing",
    })));
    act(() => result.current.addOrUpdate(makeNotification({
      status: "tool_pending",
      pendingToolCount: 1,
      sessionName: null,
    })));
    expect(result.current.notifications.get("loop-1")?.sessionName).toBe("existing");
  });

  test("backfills projectName and taskName on tool_pending", () => {
    const { result } = renderHook(() => useNotificationStore());
    act(() => result.current.addOrUpdate(makeNotification({
      status: "tool_pending",
      pendingToolCount: 1,
      projectName: null,
      taskName: null,
    })));
    act(() => result.current.addOrUpdate(makeNotification({
      status: "tool_pending",
      pendingToolCount: 1,
      projectName: "myremote",
      taskName: "fix bug",
    })));
    const stored = result.current.notifications.get("loop-1")!;
    expect(stored.projectName).toBe("myremote");
    expect(stored.taskName).toBe("fix bug");
  });
});
