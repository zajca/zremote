import { api } from "./api";

export function extractArgsPreview(
  argsJson: string | null,
  maxLen = 80,
): string | null {
  if (!argsJson) return null;
  try {
    const parsed: unknown = JSON.parse(argsJson);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      for (const value of Object.values(parsed as Record<string, unknown>)) {
        if (typeof value === "string" && value.length > 0) {
          return value.length > maxLen
            ? value.slice(0, maxLen) + "..."
            : value;
        }
      }
    }
  } catch {
    // Fall through to raw truncation
  }
  return argsJson.length > maxLen
    ? argsJson.slice(0, maxLen) + "..."
    : argsJson;
}

export function extractProjectName(projectPath: string | null): string | null {
  if (!projectPath) return null;
  const segments = projectPath.replace(/\/+$/, "").split("/");
  return segments[segments.length - 1] || null;
}

const sessionNameCache = new Map<string, string | null>();
const pendingFetches = new Set<string>();

export async function resolveSessionName(
  sessionId: string,
): Promise<string | null> {
  if (sessionNameCache.has(sessionId))
    return sessionNameCache.get(sessionId)!;
  if (pendingFetches.has(sessionId)) return null;
  pendingFetches.add(sessionId);
  try {
    const session = await api.sessions.get(sessionId);
    const name = session.name ?? null;
    sessionNameCache.set(sessionId, name);
    return name;
  } catch {
    sessionNameCache.set(sessionId, null);
    return null;
  } finally {
    pendingFetches.delete(sessionId);
  }
}

export function getCachedSessionName(sessionId: string): string | null {
  return sessionNameCache.get(sessionId) ?? null;
}

export function clearSessionNameCache(): void {
  sessionNameCache.clear();
  pendingFetches.clear();
}
