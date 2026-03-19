import { create } from "zustand";

const STORAGE_KEY = "zremote:session-mru";
const MAX_ENTRIES = 50;

function loadMru(): string[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((id): id is string => typeof id === "string");
  } catch {
    return [];
  }
}

function saveMru(list: string[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(list));
  } catch {
    // Ignore storage errors
  }
}

interface SessionMruState {
  mruList: string[];
  recordVisit: (sessionId: string) => void;
  removeSession: (sessionId: string) => void;
}

export const useSessionMruStore = create<SessionMruState>((set) => ({
  mruList: loadMru(),
  recordVisit: (sessionId) =>
    set((s) => {
      const filtered = s.mruList.filter((id) => id !== sessionId);
      const next = [sessionId, ...filtered].slice(0, MAX_ENTRIES);
      saveMru(next);
      return { mruList: next };
    }),
  removeSession: (sessionId) =>
    set((s) => {
      const next = s.mruList.filter((id) => id !== sessionId);
      saveMru(next);
      return { mruList: next };
    }),
}));
