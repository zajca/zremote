import { useCallback, useEffect, useState } from "react";
import { type Project, api } from "../lib/api";

export const PROJECT_UPDATE_EVENT = "myremote:project-update";

export function useProjects(hostId: string | undefined) {
  const [projects, setProjects] = useState<Project[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  const refetch = useCallback(async () => {
    if (!hostId) return;
    try {
      setLoading(true);
      setError(null);
      const data = await api.projects.list(hostId);
      setProjects(data);
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
      setProjects([]);
      setLoading(false);
    }
  }, [hostId, refetch]);

  useEffect(() => {
    const handler = () => {
      void refetch();
    };
    window.addEventListener(PROJECT_UPDATE_EVENT, handler);
    return () => window.removeEventListener(PROJECT_UPDATE_EVENT, handler);
  }, [refetch]);

  return { projects, loading, error, refetch };
}
