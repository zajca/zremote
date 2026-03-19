import { create } from "zustand";
import { getBrowserPermission } from "../lib/browser-notifications";

export interface ActionNotification {
  id: string;
  loopId: string;
  sessionId: string;
  hostId: string;
  hostname: string;
  toolName: string;
  status: "waiting_for_input" | "tool_pending";
  pendingToolCount: number;
  latestToolName: string | null;
  argumentsPreview: string | null;
  createdAt: number;
  sessionName: string | null;
  projectName: string | null;
  taskName: string | null;
}

// --- Pending timer tracking (outside Zustand to avoid re-renders) ---

interface PendingTimerEntry {
  timer: ReturnType<typeof setTimeout>;
  notification: ActionNotification;
}

const pendingTimers = new Map<string, PendingTimerEntry>();

function clearPendingTimer(loopId: string) {
  const entry = pendingTimers.get(loopId);
  if (entry) {
    clearTimeout(entry.timer);
    pendingTimers.delete(loopId);
  }
}

interface NotificationState {
  notifications: Map<string, ActionNotification>;
  recentlyDismissed: Map<string, number>;
  browserPermission: NotificationPermission | "unsupported";
  browserEnabled: boolean;

  addOrUpdate: (notification: ActionNotification) => void;
  scheduleToolPending: (notification: ActionNotification, delay?: number) => void;
  patchContext: (
    loopId: string,
    partial: Partial<
      Pick<ActionNotification, "sessionName" | "projectName" | "taskName">
    >,
  ) => void;
  dismiss: (loopId: string) => void;
  dismissAll: () => void;
  setBrowserEnabled: (enabled: boolean) => void;
  setBrowserPermission: (
    permission: NotificationPermission | "unsupported",
  ) => void;
  handleLoopResolved: (loopId: string) => void;
  handleToolResolved: (loopId: string) => void;
}

export const useNotificationStore = create<NotificationState>((set) => ({
  notifications: new Map(),
  recentlyDismissed: new Map(),
  browserPermission: getBrowserPermission(),
  browserEnabled: false,

  addOrUpdate: (notification) => {
    set((state) => {
      const now = Date.now();

      // Skip if recently dismissed (prevents re-add from delayed WebSocket)
      const dismissedAt = state.recentlyDismissed.get(notification.id);
      if (dismissedAt !== undefined && now - dismissedAt < 5000) {
        return state;
      }

      // Clean up stale entries
      const nextDismissed = new Map(state.recentlyDismissed);
      for (const [key, ts] of nextDismissed) {
        if (now - ts > 10_000) nextDismissed.delete(key);
      }

      const next = new Map(state.notifications);
      const existing = next.get(notification.id);
      if (existing && notification.status === "tool_pending") {
        next.set(notification.id, {
          ...existing,
          pendingToolCount: existing.pendingToolCount + 1,
          latestToolName: notification.latestToolName,
          argumentsPreview: notification.argumentsPreview,
          sessionName: existing.sessionName ?? notification.sessionName,
          projectName: existing.projectName ?? notification.projectName,
          taskName: existing.taskName ?? notification.taskName,
        });
      } else {
        next.set(notification.id, notification);
      }
      return { notifications: next, recentlyDismissed: nextDismissed };
    });
  },

  scheduleToolPending: (notification, delay = 400) => {
    const state = useNotificationStore.getState();

    // If notification is already visible in the store, update immediately
    if (state.notifications.has(notification.id)) {
      state.addOrUpdate(notification);
      return;
    }

    const existing = pendingTimers.get(notification.id);
    if (existing) {
      // Timer already running - accumulate into staged notification
      existing.notification = {
        ...existing.notification,
        pendingToolCount: existing.notification.pendingToolCount + 1,
        latestToolName: notification.latestToolName,
        argumentsPreview: notification.argumentsPreview,
        sessionName: existing.notification.sessionName ?? notification.sessionName,
        projectName: existing.notification.projectName ?? notification.projectName,
        taskName: existing.notification.taskName ?? notification.taskName,
      };
      return;
    }

    // Start a new debounce timer
    const timer = setTimeout(() => {
      const entry = pendingTimers.get(notification.id);
      pendingTimers.delete(notification.id);
      if (entry) {
        useNotificationStore.getState().addOrUpdate(entry.notification);
      }
    }, delay);

    pendingTimers.set(notification.id, { timer, notification });
  },

  patchContext: (loopId, partial) => {
    set((state) => {
      const existing = state.notifications.get(loopId);
      if (!existing) return state;
      const next = new Map(state.notifications);
      next.set(loopId, {
        ...existing,
        ...(partial.sessionName !== undefined && { sessionName: partial.sessionName }),
        ...(partial.projectName !== undefined && { projectName: partial.projectName }),
        ...(partial.taskName !== undefined && { taskName: partial.taskName }),
      });
      return { notifications: next };
    });
  },

  dismiss: (loopId) => {
    set((state) => {
      const next = new Map(state.notifications);
      next.delete(loopId);
      const nextDismissed = new Map(state.recentlyDismissed);
      nextDismissed.set(loopId, Date.now());
      return { notifications: next, recentlyDismissed: nextDismissed };
    });
  },

  dismissAll: () => { set({ notifications: new Map() }); },

  setBrowserEnabled: (enabled) => { set({ browserEnabled: enabled }); },

  setBrowserPermission: (permission) => {
    set({ browserPermission: permission });
  },

  handleLoopResolved: (loopId) => {
    clearPendingTimer(loopId);
    set((state) => {
      const next = new Map(state.notifications);
      next.delete(loopId);
      const nextDismissed = new Map(state.recentlyDismissed);
      nextDismissed.set(loopId, Date.now());
      return { notifications: next, recentlyDismissed: nextDismissed };
    });
  },

  handleToolResolved: (loopId) => {
    // Check pending timer first (notification not yet visible)
    const pending = pendingTimers.get(loopId);
    if (pending) {
      const newCount = pending.notification.pendingToolCount - 1;
      if (newCount <= 0) {
        // All tools resolved before timer fired - never show notification
        clearPendingTimer(loopId);
        return;
      }
      pending.notification.pendingToolCount = newCount;
      return;
    }

    // Notification is visible in store - update normally
    set((state) => {
      const next = new Map(state.notifications);
      const existing = next.get(loopId);
      if (!existing) return state;

      const newCount = existing.pendingToolCount - 1;
      if (newCount <= 0 && existing.status === "tool_pending") {
        next.delete(loopId);
        const nextDismissed = new Map(state.recentlyDismissed);
        nextDismissed.set(loopId, Date.now());
        return { notifications: next, recentlyDismissed: nextDismissed };
      }
      next.set(loopId, { ...existing, pendingToolCount: newCount });
      return { notifications: next };
    });
  },
}));
