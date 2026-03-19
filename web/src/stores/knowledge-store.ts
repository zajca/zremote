import { create } from "zustand";
import type {
  KnowledgeBase,
  KnowledgeMemory,
  SearchResult,
  SearchTier,
  IndexingProgress,
  MemoryCategory,
} from "../types/knowledge";
import { api } from "../lib/api";
import { showToast } from "../components/layout/Toast";

interface KnowledgeState {
  // State
  statusByProject: Record<string, KnowledgeBase | null>;
  memoriesByProject: Record<string, KnowledgeMemory[]>;
  searchResults: SearchResult[];
  searchLoading: boolean;
  indexingProgress: Record<string, IndexingProgress>;
  bootstrapStatus: Record<string, "idle" | "running" | "done" | "error">;

  // Actions
  fetchStatus: (projectId: string) => Promise<void>;
  fetchMemories: (
    projectId: string,
    category?: MemoryCategory,
  ) => Promise<void>;
  search: (projectId: string, query: string, tier?: string) => Promise<void>;
  triggerIndex: (projectId: string, force?: boolean) => Promise<void>;
  extractMemories: (projectId: string, loopId: string) => Promise<void>;
  deleteMemory: (projectId: string, memoryId: string) => Promise<void>;
  updateMemory: (
    projectId: string,
    memoryId: string,
    data: { content?: string; category?: string },
  ) => Promise<void>;
  controlService: (
    hostId: string,
    action: "start" | "stop" | "restart",
  ) => Promise<void>;
  generateInstructions: (projectId: string) => Promise<void>;
  bootstrapProject: (projectId: string) => Promise<void>;

  // Event handlers (called from WebSocket event listener)
  handleKnowledgeStatusChanged: (
    hostId: string,
    status: string,
    error: string | null,
  ) => void;
  handleIndexingProgress: (progress: IndexingProgress) => void;
  handleMemoryExtracted: (projectId: string) => void;
}

export const useKnowledgeStore = create<KnowledgeState>((set, get) => ({
  statusByProject: {},
  memoriesByProject: {},
  searchResults: [],
  searchLoading: false,
  indexingProgress: {},
  bootstrapStatus: {},

  fetchStatus: async (projectId) => {
    try {
      const status = await api.knowledge.getStatus(projectId);
      set((state) => ({
        statusByProject: { ...state.statusByProject, [projectId]: status },
      }));
    } catch (e) {
      console.error("Failed to fetch knowledge status:", e);
    }
  },

  fetchMemories: async (projectId, category) => {
    try {
      const memories = await api.knowledge.listMemories(projectId, category);
      set((state) => ({
        memoriesByProject: {
          ...state.memoriesByProject,
          [projectId]: memories,
        },
      }));
    } catch (e) {
      console.error("Failed to fetch memories:", e);
    }
  },

  search: async (projectId, query, tier) => {
    set({ searchLoading: true });
    try {
      const response = await api.knowledge.search(
        projectId,
        query,
        tier as SearchTier,
      );
      set({ searchResults: response.results, searchLoading: false });
    } catch (e) {
      console.error("Search failed:", e);
      set({ searchResults: [], searchLoading: false });
    }
  },

  triggerIndex: async (projectId, force = false) => {
    try {
      await api.knowledge.triggerIndex(projectId, force);
    } catch (e) {
      console.error("Failed to trigger indexing:", e);
      throw e;
    }
  },

  extractMemories: async (projectId, loopId) => {
    try {
      await api.knowledge.extractMemories(projectId, loopId);
    } catch (e) {
      console.error("Failed to extract memories:", e);
      throw e;
    }
  },

  deleteMemory: async (projectId, memoryId) => {
    try {
      await api.knowledge.deleteMemory(projectId, memoryId);
      get().fetchMemories(projectId);
      showToast("Memory deleted", "success");
    } catch (e) {
      console.error("Failed to delete memory:", e);
      showToast("Failed to delete memory", "error");
      throw e;
    }
  },

  updateMemory: async (projectId, memoryId, data) => {
    try {
      const updated = await api.knowledge.updateMemory(
        projectId,
        memoryId,
        data,
      );
      set((state) => ({
        memoriesByProject: {
          ...state.memoriesByProject,
          [projectId]: (state.memoriesByProject[projectId] || []).map((m) =>
            m.id === memoryId ? updated : m,
          ),
        },
      }));
      showToast("Memory updated", "success");
    } catch (e) {
      console.error("Failed to update memory:", e);
      showToast("Failed to update memory", "error");
      throw e;
    }
  },

  controlService: async (hostId, action) => {
    try {
      await api.knowledge.controlService(hostId, action);
    } catch (e) {
      console.error("Failed to control service:", e);
      throw e;
    }
  },

  generateInstructions: async (projectId) => {
    try {
      await api.knowledge.generateInstructions(projectId);
    } catch (e) {
      console.error("Failed to generate instructions:", e);
      throw e;
    }
  },

  bootstrapProject: async (projectId) => {
    set((state) => ({
      bootstrapStatus: { ...state.bootstrapStatus, [projectId]: "running" },
    }));
    try {
      await api.knowledge.bootstrapProject(projectId);
      set((state) => ({
        bootstrapStatus: { ...state.bootstrapStatus, [projectId]: "done" },
      }));
      // Refresh status after bootstrap
      setTimeout(() => get().fetchStatus(projectId), 3000);
    } catch (e) {
      console.error("Failed to bootstrap project:", e);
      set((state) => ({
        bootstrapStatus: { ...state.bootstrapStatus, [projectId]: "error" },
      }));
      throw e;
    }
  },

  handleKnowledgeStatusChanged: (_hostId, _status, _error) => {
    // Refresh all project statuses when KB status changes
    // Could be optimized to only refresh projects for this host
  },

  handleIndexingProgress: (progress) => {
    set((state) => ({
      indexingProgress: {
        ...state.indexingProgress,
        [progress.project_id]: progress,
      },
    }));
  },

  handleMemoryExtracted: (projectId) => {
    get().fetchMemories(projectId);
  },
}));
