import { create } from "zustand";
import { api } from "../lib/api";
import type {
  AgenticLoop,
  ToolCall,
  TranscriptEntry,
  UserAction,
} from "../types/agentic";

interface AgenticState {
  activeLoops: Map<string, AgenticLoop>;
  toolCalls: Map<string, ToolCall[]>;
  transcripts: Map<string, TranscriptEntry[]>;

  updateLoop: (loop: AgenticLoop) => void;
  removeLoop: (loopId: string) => void;
  addToolCall: (loopId: string, toolCall: ToolCall) => void;
  updateToolCall: (loopId: string, toolCall: ToolCall) => void;
  addTranscript: (loopId: string, entry: TranscriptEntry) => void;

  fetchLoop: (loopId: string) => Promise<void>;
  fetchToolCalls: (loopId: string) => Promise<void>;
  fetchTranscript: (loopId: string) => Promise<void>;
  sendAction: (
    loopId: string,
    action: UserAction,
    payload?: string,
  ) => Promise<void>;
}

export const useAgenticStore = create<AgenticState>((set, get) => ({
  activeLoops: new Map(),
  toolCalls: new Map(),
  transcripts: new Map(),

  updateLoop: (loop) =>
    set((state) => {
      const next = new Map(state.activeLoops);
      next.set(loop.id, loop);
      return { activeLoops: next };
    }),

  removeLoop: (loopId) =>
    set((state) => {
      const nextLoops = new Map(state.activeLoops);
      nextLoops.delete(loopId);
      const nextTools = new Map(state.toolCalls);
      nextTools.delete(loopId);
      const nextTranscripts = new Map(state.transcripts);
      nextTranscripts.delete(loopId);
      return {
        activeLoops: nextLoops,
        toolCalls: nextTools,
        transcripts: nextTranscripts,
      };
    }),

  addToolCall: (loopId, toolCall) =>
    set((state) => {
      const next = new Map(state.toolCalls);
      const existing = next.get(loopId) ?? [];
      next.set(loopId, [...existing, toolCall]);
      return { toolCalls: next };
    }),

  updateToolCall: (loopId, toolCall) =>
    set((state) => {
      const next = new Map(state.toolCalls);
      const existing = next.get(loopId) ?? [];
      const idx = existing.findIndex((tc) => tc.id === toolCall.id);
      if (idx >= 0) {
        const updated = [...existing];
        updated[idx] = toolCall;
        next.set(loopId, updated);
      } else {
        next.set(loopId, [...existing, toolCall]);
      }
      return { toolCalls: next };
    }),

  addTranscript: (loopId, entry) =>
    set((state) => {
      const next = new Map(state.transcripts);
      const existing = next.get(loopId) ?? [];
      next.set(loopId, [...existing, entry]);
      return { transcripts: next };
    }),

  fetchLoop: async (loopId) => {
    const loop = await api.loops.get(loopId);
    get().updateLoop(loop);
  },

  fetchToolCalls: async (loopId) => {
    const calls = await api.loops.tools(loopId);
    set((state) => {
      const next = new Map(state.toolCalls);
      next.set(loopId, calls);
      return { toolCalls: next };
    });
  },

  fetchTranscript: async (loopId) => {
    const entries = await api.loops.transcript(loopId);
    set((state) => {
      const next = new Map(state.transcripts);
      next.set(loopId, entries);
      return { transcripts: next };
    });
  },

  sendAction: async (loopId, action, payload) => {
    await api.loops.action(loopId, action, payload);
  },
}));
