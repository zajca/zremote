import { useCallback, useEffect, useState } from "react";
import { api } from "../lib/api";
import type { AgenticLoop } from "../types/agentic";

export const AGENTIC_LOOP_UPDATE_EVENT = "myremote:agentic-loop-update";

export function useAgenticLoops(sessionId: string | undefined) {
  const [loops, setLoops] = useState<AgenticLoop[]>([]);
  const [loading, setLoading] = useState(false);

  const refetch = useCallback(async () => {
    if (!sessionId) return;
    try {
      setLoading(true);
      const data = await api.loops.list({ session_id: sessionId });
      setLoops(data);
    } catch {
      // Silently fail -- loops are supplementary info
    } finally {
      setLoading(false);
    }
  }, [sessionId]);

  useEffect(() => {
    if (sessionId) {
      void refetch();
    } else {
      setLoops([]);
    }
  }, [sessionId, refetch]);

  useEffect(() => {
    if (!sessionId) return;
    const handler = () => void refetch();
    window.addEventListener(AGENTIC_LOOP_UPDATE_EVENT, handler);
    return () => window.removeEventListener(AGENTIC_LOOP_UPDATE_EVENT, handler);
  }, [sessionId, refetch]);

  return { loops, loading, refetch };
}
