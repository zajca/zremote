import { create } from "zustand";
import { api } from "../lib/api";
import type { ClaudeTask, ClaudeTaskStatus } from "../types/claude-session";

interface ClaudeTaskState {
  tasks: Map<string, ClaudeTask>;
  sessionTaskIndex: Map<string, string>;

  updateTask: (task: ClaudeTask) => void;
  removeTask: (taskId: string) => void;

  fetchTask: (taskId: string) => Promise<void>;
  fetchTasks: (filters?: { host_id?: string; status?: string; project_id?: string }) => Promise<void>;

  handleTaskStarted: (data: {
    task_id: string;
    session_id: string;
    host_id: string;
    project_path: string;
  }) => void;
  handleTaskUpdated: (data: {
    task_id: string;
    status: string;
    loop_id: string | null;
  }) => void;
  handleTaskEnded: (data: {
    task_id: string;
    status: string;
    summary: string | null;
    total_cost_usd: number;
  }) => void;
}

export const useClaudeTaskStore = create<ClaudeTaskState>((set, get) => ({
  tasks: new Map(),
  sessionTaskIndex: new Map(),

  updateTask: (task) =>
    set((state) => {
      const next = new Map(state.tasks);
      next.set(task.id, task);
      const nextIndex = new Map(state.sessionTaskIndex);
      nextIndex.set(task.session_id, task.id);
      return { tasks: next, sessionTaskIndex: nextIndex };
    }),

  removeTask: (taskId) =>
    set((state) => {
      const task = state.tasks.get(taskId);
      const next = new Map(state.tasks);
      next.delete(taskId);
      const nextIndex = new Map(state.sessionTaskIndex);
      if (task) nextIndex.delete(task.session_id);
      return { tasks: next, sessionTaskIndex: nextIndex };
    }),

  fetchTask: async (taskId) => {
    const task = await api.claudeTasks.get(taskId);
    get().updateTask(task);
  },

  fetchTasks: async (filters) => {
    const tasks = await api.claudeTasks.list(filters);
    set((state) => {
      const next = new Map(state.tasks);
      const nextIndex = new Map(state.sessionTaskIndex);
      for (const task of tasks) {
        next.set(task.id, task);
        nextIndex.set(task.session_id, task.id);
      }
      return { tasks: next, sessionTaskIndex: nextIndex };
    });
  },

  handleTaskStarted: (data) => {
    // Fetch full task from server since the event only has partial data
    void get().fetchTask(data.task_id);
    window.dispatchEvent(new Event("zremote:claude-task-update"));
  },

  handleTaskUpdated: (data) =>
    set((state) => {
      const existing = state.tasks.get(data.task_id);
      if (!existing) {
        // Unknown task, fetch it
        void get().fetchTask(data.task_id);
        window.dispatchEvent(new Event("zremote:claude-task-update"));
        return state;
      }
      const next = new Map(state.tasks);
      next.set(data.task_id, {
        ...existing,
        status: data.status as ClaudeTaskStatus,
        loop_id: data.loop_id ?? existing.loop_id,
      });
      window.dispatchEvent(new Event("zremote:claude-task-update"));
      return { tasks: next };
    }),

  handleTaskEnded: (data) =>
    set((state) => {
      const existing = state.tasks.get(data.task_id);
      if (!existing) {
        void get().fetchTask(data.task_id);
        window.dispatchEvent(new Event("zremote:claude-task-update"));
        return state;
      }
      const next = new Map(state.tasks);
      next.set(data.task_id, {
        ...existing,
        status: data.status as ClaudeTaskStatus,
        summary: data.summary ?? existing.summary,
        total_cost_usd: data.total_cost_usd,
        ended_at: new Date().toISOString(),
      });
      window.dispatchEvent(new Event("zremote:claude-task-update"));
      return { tasks: next };
    }),
}));
