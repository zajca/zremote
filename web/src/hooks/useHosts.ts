import { useCallback, useEffect, useRef, useState } from "react";
import { type Host, api } from "../lib/api";

export function useHosts() {
  const [hosts, setHosts] = useState<Host[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  const refetch = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const data = await api.hosts.list();
      setHosts(data);
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)));
    } finally {
      setLoading(false);
    }
  }, []);

  const refetchRef = useRef(refetch);
  refetchRef.current = refetch;

  useEffect(() => {
    void refetchRef.current();
  }, []);

  return { hosts, loading, error, refetch };
}
