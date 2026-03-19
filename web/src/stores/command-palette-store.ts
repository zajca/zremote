import { create } from "zustand";
import type { PaletteContext } from "../components/command-palette/types";

interface CommandPaletteState {
  open: boolean;
  setOpen: (open: boolean) => void;
  toggle: () => void;

  contextStack: PaletteContext[];
  pushContext: (ctx: PaletteContext) => void;
  popContext: () => void;
  jumpToIndex: (index: number) => void;
  resetToRouteContext: (ctx: PaletteContext) => void;

  query: string;
  setQuery: (q: string) => void;

  filterMode: "sessions" | null;
  openWithFilter: (mode: "sessions") => void;

  currentContext: () => PaletteContext;
}

const DEFAULT_CONTEXT: PaletteContext = { level: "global" };

export const useCommandPaletteStore = create<CommandPaletteState>((set, get) => ({
  open: false,
  setOpen: (open) => set(open ? { open } : { open, filterMode: null }),
  toggle: () => set((s) => ({ open: !s.open })),

  contextStack: [DEFAULT_CONTEXT],
  pushContext: (ctx) =>
    set((s) => ({ contextStack: [...s.contextStack, ctx], query: "", filterMode: null })),
  popContext: () =>
    set((s) => {
      if (s.contextStack.length <= 1) return s;
      return { contextStack: s.contextStack.slice(0, -1), query: "" };
    }),
  jumpToIndex: (index) =>
    set((s) => {
      if (index < 0 || index >= s.contextStack.length) return s;
      return { contextStack: s.contextStack.slice(0, index + 1), query: "" };
    }),
  resetToRouteContext: (ctx) => set({ contextStack: [ctx], query: "" }),

  query: "",
  setQuery: (q) => set({ query: q }),

  filterMode: null,
  openWithFilter: (mode) => set({ filterMode: mode, open: true, query: "" }),

  currentContext: () => {
    const stack = get().contextStack;
    return stack[stack.length - 1] ?? DEFAULT_CONTEXT;
  },
}));
