import { useCallback, useEffect, useRef, useState } from "react";
import { type Session, api } from "../lib/api";

export const SESSION_UPDATE_EVENT = "zremote:session-update";

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

  const refetchRef = useRef(refetch);
  refetchRef.current = refetch;

  useEffect(() => {
    if (hostId) {
      void refetchRef.current();
    } else {
      setSessions([]);
      setLoading(false);
    }
  }, [hostId]);

  // Listen for real-time session update events
  useEffect(() => {
    if (!hostId) return;
    const handler = () => void refetchRef.current();
    window.addEventListener(SESSION_UPDATE_EVENT, handler);
    return () => window.removeEventListener(SESSION_UPDATE_EVENT, handler);
  }, [hostId]);

  return { sessions, loading, error, refetch };
}
