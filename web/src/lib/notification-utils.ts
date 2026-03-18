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
