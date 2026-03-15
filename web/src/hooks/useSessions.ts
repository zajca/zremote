import { useCallback, useEffect, useState } from "react";
import { type Session, api } from "../lib/api";

export const SESSION_UPDATE_EVENT = "myremote:session-update";

export function useSessions(hostId: string | undefined) {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  const refetch = useCallback(async () => {
    if (!hostId) return;
    try {
      setLoading(true);
      setError(null);
      const data = await api.sessions.list(hostId);
      setSessions(data);
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)));
    } finally {
      setLoading(false);
    }
  }, [hostId]);

  useEffect(() => {
    if (hostId) {
      void refetch();
    } else {
      setSessions([]);
      setLoading(false);
    }
  }, [hostId, refetch]);

  // Listen for real-time session update events
  useEffect(() => {
    if (!hostId) return;

    const handler = () => {
      void refetch();
    };
    window.addEventListener(SESSION_UPDATE_EVENT, handler);
    return () => window.removeEventListener(SESSION_UPDATE_EVENT, handler);
  }, [hostId, refetch]);

  return { sessions, loading, error, refetch };
}
