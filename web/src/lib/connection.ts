export type AppMode = "server" | "local";

let cachedMode: AppMode | null = null;

export async function detectMode(): Promise<AppMode> {
  if (cachedMode) return cachedMode;
  try {
    const res = await fetch("/api/mode");
    const data: { mode: string } = await res.json();
    if (data.mode === "local" || data.mode === "server") {
      cachedMode = data.mode;
      return data.mode;
    }
    return "server";
  } catch {
    return "server"; // fallback
  }
}
