import { useCallback, useEffect, useMemo, useState } from "react";
import { type Host, api } from "../lib/api";
import { useSessionMruStore } from "../stores/session-mru-store";
import { useAgenticStore } from "../stores/agentic-store";

export interface ShortcutSession {
  sessionId: string;
  hostId: string;
  name: string;
  hostName?: string;
  projectName?: string;
  workingDir?: string;
  status: "active" | "suspended";
  hasAgenticLoop?: boolean;
  projectId?: string;
}

export function useShortcutSessions(
  hosts: Host[],
  isLocal: boolean,
): ShortcutSession[] {
  const [sessions, setSessions] = useState<ShortcutSession[]>([]);
  const mruList = useSessionMruStore((s) => s.mruList);
  const activeLoops = useAgenticStore((s) => s.activeLoops);

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
                workingDir: s.working_dir ?? undefined,
                status: s.status as "active" | "suspended",
                projectId: s.project_id ?? undefined,
              }),
            );
        }),
      );

      const allSessions = results.flat();

      // Batch fetch project names for sessions with project_ids
      const uniqueProjectIds = [
        ...new Set(
          allSessions
            .map((s) => s.projectId)
            .filter((id): id is string => id != null),
        ),
      ];

      const projectNameMap = new Map<string, string>();
      if (uniqueProjectIds.length > 0) {
        const projectResults = await Promise.allSettled(
          uniqueProjectIds.map((id) => api.projects.get(id)),
        );
        for (const result of projectResults) {
          if (result.status === "fulfilled") {
            projectNameMap.set(result.value.id, result.value.name);
          }
        }
      }

      // Populate project names
      for (const s of allSessions) {
        if (s.projectId) {
          s.projectName = projectNameMap.get(s.projectId);
        }
      }

      setSessions(allSessions);
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

  // Check agentic loop status for each session
  const sessionsWithAgentic = useMemo(() => {
    return sessions.map((s) => {
      const hasLoop = Array.from(activeLoops.values()).some(
        (loop) => loop.session_id === s.sessionId && loop.status === "working",
      );
      return { ...s, hasAgenticLoop: hasLoop };
    });
  }, [sessions, activeLoops]);

  // Sort: MRU first, then active before suspended
  return useMemo(() => {
    return [...sessionsWithAgentic].sort((a, b) => {
      const aIdx = mruList.indexOf(a.sessionId);
      const bIdx = mruList.indexOf(b.sessionId);
      if (aIdx !== -1 && bIdx !== -1) return aIdx - bIdx;
      if (aIdx !== -1) return -1;
      if (bIdx !== -1) return 1;
      // Neither in MRU: active before suspended
      if (a.status !== b.status) {
        return a.status === "active" ? -1 : 1;
      }
      return 0;
    });
  }, [sessionsWithAgentic, mruList]);
}
