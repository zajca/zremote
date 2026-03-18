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
}

interface NotificationState {
  notifications: Map<string, ActionNotification>;
  browserPermission: NotificationPermission | "unsupported";
  browserEnabled: boolean;

  addOrUpdate: (notification: ActionNotification) => void;
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
  browserPermission: getBrowserPermission(),
  browserEnabled: false,

  addOrUpdate: (notification) => {
    set((state) => {
      const next = new Map(state.notifications);
      const existing = next.get(notification.id);
      if (existing && notification.status === "tool_pending") {
        next.set(notification.id, {
          ...existing,
          pendingToolCount: existing.pendingToolCount + 1,
          latestToolName: notification.latestToolName,
          argumentsPreview: notification.argumentsPreview,
        });
      } else {
        next.set(notification.id, notification);
      }
      return { notifications: next };
    });
  },

  dismiss: (loopId) => {
    set((state) => {
      const next = new Map(state.notifications);
      next.delete(loopId);
      return { notifications: next };
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
      return { notifications: next };
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
      } else {
        next.set(loopId, { ...existing, pendingToolCount: newCount });
      }
      return { notifications: next };
    });
  },
}));
