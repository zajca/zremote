import { create } from "zustand";

interface ActiveTerminalState {
  sessionId: string | null;
  sendInput: ((data: string) => void) | null;
  register: (sessionId: string, sender: (data: string) => void) => void;
  unregister: (sessionId: string) => void;
}

export const useActiveTerminalStore = create<ActiveTerminalState>((set) => ({
  sessionId: null,
  sendInput: null,

  register: (sessionId, sender) =>
    set({ sessionId, sendInput: sender }),

  unregister: (sessionId) =>
    set((s) => {
      if (s.sessionId !== sessionId) return s;
      return { sessionId: null, sendInput: null };
    }),
}));
