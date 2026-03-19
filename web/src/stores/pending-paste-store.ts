import { create } from "zustand";

interface PendingPaste {
  sessionId: string;
  data: string;
}

interface PendingPasteState {
  pendingPaste: PendingPaste | null;
  setPendingPaste: (sessionId: string, data: string) => void;
  consume: (sessionId: string) => string | null;
}

export const usePendingPasteStore = create<PendingPasteState>((set, get) => ({
  pendingPaste: null,
  setPendingPaste: (sessionId, data) => set({ pendingPaste: { sessionId, data } }),
  consume: (sessionId) => {
    const { pendingPaste } = get();
    if (pendingPaste && pendingPaste.sessionId === sessionId) {
      set({ pendingPaste: null });
      return pendingPaste.data;
    }
    return null;
  },
}));
