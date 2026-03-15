import { ArrowLeft } from "lucide-react";
import { Link, useParams } from "react-router";
import { AgenticLoopPanel } from "../components/agentic/AgenticLoopPanel";

export function AgenticLoopPage() {
  const { hostId, sessionId, loopId } = useParams<{
    hostId: string;
    sessionId: string;
    loopId: string;
  }>();

  if (!loopId) {
    return (
      <div className="flex h-full items-center justify-center text-text-secondary">
        Loop not found
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b border-border px-4 py-2">
        <Link
          to={`/hosts/${hostId}/sessions/${sessionId}`}
          className="text-text-tertiary transition-colors duration-150 hover:text-text-primary"
        >
          <ArrowLeft size={16} />
        </Link>
        <span className="text-xs text-text-tertiary">
          Back to session
        </span>
      </div>
      <div className="min-h-0 flex-1">
        <AgenticLoopPanel loopId={loopId} />
      </div>
    </div>
  );
}
