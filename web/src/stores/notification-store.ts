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

interface NotificationState {
  notifications: Map<string, ActionNotification>;
  recentlyDismissed: Map<string, number>;
  browserPermission: NotificationPermission | "unsupported";
  browserEnabled: boolean;

  addOrUpdate: (notification: ActionNotification) => void;
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
    set((state) => {
      const next = new Map(state.notifications);
      next.delete(loopId);
      const nextDismissed = new Map(state.recentlyDismissed);
      nextDismissed.set(loopId, Date.now());
      return { notifications: next, recentlyDismissed: nextDismissed };
    });
  },

  handleToolResolved: (loopId) => {
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
