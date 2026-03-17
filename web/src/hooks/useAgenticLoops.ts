import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import type { AgenticLoop } from "../types/agentic";

export const AGENTIC_LOOP_UPDATE_EVENT = "zremote:agentic-loop-update";

export function useAgenticLoops(sessionId: string | undefined) {
  const [loops, setLoops] = useState<AgenticLoop[]>([]);
  const [loading, setLoading] = useState(false);

  const refetch = useCallback(async () => {
    if (!sessionId) return;
    try {
      setLoading(true);
      const data = await api.loops.list({ session_id: sessionId });
      setLoops(data);
    } catch (e) {
      console.warn("Failed to fetch agentic loops:", e);
    } finally {
      setLoading(false);
    }
  }, [sessionId]);

  const refetchRef = useRef(refetch);
  refetchRef.current = refetch;

  useEffect(() => {
    if (sessionId) {
      void refetchRef.current();
    } else {
      setLoops([]);
    }
  }, [sessionId]);

  useEffect(() => {
    if (!sessionId) return;
    const handler = () => void refetchRef.current();
    window.addEventListener(AGENTIC_LOOP_UPDATE_EVENT, handler);
    return () => window.removeEventListener(AGENTIC_LOOP_UPDATE_EVENT, handler);
  }, [sessionId]);

  // Fallback polling every 15s for active sessions
  useEffect(() => {
    if (!sessionId) return;
    const interval = setInterval(() => void refetchRef.current(), 15_000);
    return () => clearInterval(interval);
  }, [sessionId]);

  return { loops, loading, refetch };
}
