import { Monitor, Terminal } from "lucide-react";
import { Link } from "react-router";
import { useHosts } from "../hooks/useHosts";

export function WelcomePage() {
  const { hosts } = useHosts();
  const firstHost = hosts[0];

  return (
    <div className="flex h-full items-center justify-center">
      <div className="max-w-md space-y-6 text-center">
        <div className="flex justify-center">
          <div className="rounded-xl bg-bg-secondary p-4">
            <Monitor size={40} className="text-accent" />
          </div>
        </div>
        <div className="space-y-2">
          <h1 className="text-2xl font-semibold text-text-primary">
            Welcome to MyRemote
          </h1>
          <p className="text-sm text-text-secondary">
            Connect remote agents to manage terminal sessions from your browser.
          </p>
        </div>
        <div className="space-y-3 rounded-lg border border-border bg-bg-secondary p-4 text-left">
          <div className="flex items-center gap-2 text-sm font-medium text-text-primary">
            <Terminal size={16} className="text-accent" />
            Connect your first agent
          </div>
          <pre className="overflow-x-auto rounded bg-bg-tertiary p-3 font-mono text-xs text-text-secondary">
            {`export MYREMOTE_SERVER=http://localhost:3000
export MYREMOTE_TOKEN=<your-token>

myremote-agent`}
          </pre>
        </div>
        {firstHost && (
          <Link
            to={`/hosts/${firstHost.id}`}
            className="inline-flex items-center gap-2 text-sm text-accent transition-colors duration-150 hover:text-accent-hover"
          >
            Go to {firstHost.hostname}
          </Link>
        )}
      </div>
    </div>
  );
}
