import { useLocation } from "react-router";
import { useMemo } from "react";
import type { PaletteContext } from "../components/command-palette/types";

export function useCommandPaletteContext(): PaletteContext {
  const { pathname } = useLocation();

  return useMemo(() => {
    // /hosts/:hostId/sessions/:sessionId
    let match = pathname.match(/^\/hosts\/([^/]+)\/sessions\/([^/]+)/);
    if (match) return { level: "session" as const, hostId: match[1], sessionId: match[2] };

    // /hosts/:hostId
    match = pathname.match(/^\/hosts\/([^/]+)$/);
    if (match) return { level: "host" as const, hostId: match[1] };

    // /projects/:projectId (project or worktree - resolved later when project data is available)
    match = pathname.match(/^\/projects\/([^/]+)/);
    if (match) return { level: "project" as const, projectId: match[1] };

    // Everything else: global
    return { level: "global" as const };
  }, [pathname]);
}
