import { create } from "zustand";
import { api } from "../lib/api";
import type { ClaudeTask } from "../types/claude-session";

interface ClaudeTaskState {
  tasks: Map<string, ClaudeTask>;

  updateTask: (task: ClaudeTask) => void;
  removeTask: (taskId: string) => void;

  fetchTask: (taskId: string) => Promise<void>;
  fetchTasks: (filters?: { host_id?: string; status?: string; project_id?: string }) => Promise<void>;
}

export const useClaudeTaskStore = create<ClaudeTaskState>((set, get) => ({
  tasks: new Map(),

  updateTask: (task) =>
    set((state) => {
      const next = new Map(state.tasks);
      next.set(task.id, task);
      return { tasks: next };
    }),

  removeTask: (taskId) =>
    set((state) => {
      const next = new Map(state.tasks);
      next.delete(taskId);
      return { tasks: next };
    }),

  fetchTask: async (taskId) => {
    const task = await api.claudeTasks.get(taskId);
    get().updateTask(task);
  },

  fetchTasks: async (filters) => {
    const tasks = await api.claudeTasks.list(filters);
    set((state) => {
      const next = new Map(state.tasks);
      for (const task of tasks) {
        next.set(task.id, task);
      }
      return { tasks: next };
    });
  },
}));
