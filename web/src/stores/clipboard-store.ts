import { create } from "zustand";

const STORAGE_KEY = "zremote:clipboard-history";
const MAX_ENTRIES = 30;
const MAX_TEXT_LENGTH = 5000;
const PREVIEW_LENGTH = 100;

export interface ClipboardEntry {
  id: string;
  text: string;
  preview: string;
  timestamp: number;
  source: {
    sessionId: string;
    sessionName?: string;
  };
}

interface ClipboardState {
  entries: ClipboardEntry[];
  addEntry: (text: string, source: { sessionId: string; sessionName?: string }) => void;
  removeEntry: (id: string) => void;
  clearAll: () => void;
}

function loadEntries(): ClipboardEntry[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (e): e is ClipboardEntry =>
        typeof e === "object" &&
        e !== null &&
        typeof e.id === "string" &&
        typeof e.text === "string" &&
        typeof e.timestamp === "number" &&
        typeof e.source === "object" &&
        e.source !== null &&
        typeof e.source.sessionId === "string",
    );
  } catch {
    return [];
  }
}

function saveEntries(entries: ClipboardEntry[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
    // Ignore storage errors
  }
}

function makePreview(text: string): string {
  return text.replace(/\n/g, " ").slice(0, PREVIEW_LENGTH);
}

function truncateText(text: string): string {
  if (text.length <= MAX_TEXT_LENGTH) return text;
  return text.slice(0, MAX_TEXT_LENGTH) + "... (truncated)";
}

export const useClipboardStore = create<ClipboardState>((set) => ({
  entries: loadEntries(),

  addEntry: (text, source) =>
    set((s) => {
      const trimmed = text.trim();
      if (!trimmed) return s;

      // Deduplicate: if last entry has same text, update timestamp
      const last = s.entries[0];
      if (last && last.text === truncateText(trimmed)) {
        const updated = [{ ...last, timestamp: Date.now() }, ...s.entries.slice(1)];
        saveEntries(updated);
        return { entries: updated };
      }

      const entry: ClipboardEntry = {
        id: crypto.randomUUID(),
        text: truncateText(trimmed),
        preview: makePreview(trimmed),
        timestamp: Date.now(),
        source,
      };

      const next = [entry, ...s.entries].slice(0, MAX_ENTRIES);
      saveEntries(next);
      return { entries: next };
    }),

  removeEntry: (id) =>
    set((s) => {
      const next = s.entries.filter((e) => e.id !== id);
      saveEntries(next);
      return { entries: next };
    }),

  clearAll: () =>
    set(() => {
      saveEntries([]);
      return { entries: [] };
    }),
}));
