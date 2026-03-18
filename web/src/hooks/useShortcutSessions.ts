import { useCallback, useEffect, useMemo, useState } from "react";
import { type Host, api } from "../lib/api";
import { useSessionMruStore } from "../stores/session-mru-store";

export interface ShortcutSession {
  sessionId: string;
  hostId: string;
  name: string;
  hostName?: string;
}

export function useShortcutSessions(
  hosts: Host[],
  isLocal: boolean,
): ShortcutSession[] {
  const [sessions, setSessions] = useState<ShortcutSession[]>([]);
  const mruList = useSessionMruStore((s) => s.mruList);

  const fetchSessions = useCallback(async () => {
    const onlineHosts = hosts.filter((h) => h.status === "online");
    if (onlineHosts.length === 0) {
      setSessions([]);
      return;
    }

    try {
      const results = await Promise.all(
        onlineHosts.map(async (host) => {
          const hostSessions = await api.sessions.list(host.id);
          return hostSessions
            .filter(
              (s) => s.status === "active" || s.status === "suspended",
            )
            .map(
              (s): ShortcutSession => ({
                sessionId: s.id,
                hostId: host.id,
                name: s.name ?? `Session ${s.id.slice(0, 8)}`,
                hostName: isLocal ? undefined : host.hostname,
              }),
            );
        }),
      );
      setSessions(results.flat());
    } catch {
      // Ignore fetch errors
    }
  }, [hosts, isLocal]);

  useEffect(() => {
    void fetchSessions();
  }, [fetchSessions]);

  // Refresh on session events
  useEffect(() => {
    function handleUpdate() {
      void fetchSessions();
    }
    window.addEventListener("zremote:session-update", handleUpdate);
    return () =>
      window.removeEventListener("zremote:session-update", handleUpdate);
  }, [fetchSessions]);

  // Sort by MRU order
  return useMemo(() => {
    return [...sessions].sort((a, b) => {
      const aIdx = mruList.indexOf(a.sessionId);
      const bIdx = mruList.indexOf(b.sessionId);
      if (aIdx !== -1 && bIdx !== -1) return aIdx - bIdx;
      if (aIdx !== -1) return -1;
      if (bIdx !== -1) return 1;
      return 0;
    });
  }, [sessions, mruList]);
}
