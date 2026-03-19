import { X, Globe, Laptop, FolderGit2, GitBranch, Terminal, Brain, HelpCircle } from "lucide-react";
import type { LucideIcon } from "lucide-react";

interface HelpModalProps {
  open: boolean;
  onClose: () => void;
}

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.userAgent);
const mod = isMac ? "Cmd" : "Ctrl";

function Kbd({ children }: { children: React.ReactNode }) {
  return (
    <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px]">
      {children}
    </kbd>
  );
}

function ShortcutRow({ keys, description }: { keys: React.ReactNode; description: string }) {
  return (
    <div className="flex items-center justify-between gap-4 py-1">
      <span className="text-sm text-text-secondary">{description}</span>
      <div className="flex shrink-0 items-center gap-1">{keys}</div>
    </div>
  );
}

function ContextCard({
  icon: Icon,
  title,
  description,
  actions,
}: {
  icon: LucideIcon;
  title: string;
  description: string;
  actions: string[];
}) {
  return (
    <div className="rounded-lg border border-border bg-bg-secondary p-3">
      <div className="mb-1 flex items-center gap-2">
        <Icon size={14} className="text-accent" />
        <span className="text-sm font-medium text-text-primary">{title}</span>
      </div>
      <p className="mb-2 text-xs text-text-tertiary">{description}</p>
      <div className="flex flex-wrap gap-1">
        {actions.map((a) => (
          <span
            key={a}
            className="rounded bg-bg-tertiary px-1.5 py-0.5 text-[10px] text-text-secondary"
          >
            {a}
          </span>
        ))}
      </div>
    </div>
  );
}

export function HelpModal({ open, onClose }: HelpModalProps) {
  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        className="max-h-[85vh] w-full max-w-2xl overflow-y-auto rounded-lg border border-border bg-bg-primary p-6 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="mb-6 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <HelpCircle size={18} className="text-accent" />
            <h2 className="text-lg font-semibold text-text-primary">
              Help
            </h2>
          </div>
          <button
            onClick={onClose}
            className="rounded p-1 text-text-tertiary transition-colors hover:bg-bg-hover hover:text-text-primary"
            aria-label="Close help"
          >
            <X size={16} />
          </button>
        </div>

        {/* Section 1: Keyboard Shortcuts */}
        <section className="mb-6">
          <h3 className="mb-3 text-sm font-semibold text-text-primary">
            Keyboard Shortcuts
          </h3>

          {/* Global Shortcuts */}
          <div className="mb-4">
            <h4 className="mb-2 text-xs font-medium text-text-tertiary">
              Global Shortcuts
            </h4>
            <div className="space-y-0.5">
              <ShortcutRow
                keys={<><Kbd>{mod}+K</Kbd><span className="text-xs text-text-tertiary">/</span><Kbd>Shift Shift</Kbd></>}
                description="Toggle command palette"
              />
              <ShortcutRow
                keys={<><Kbd>{mod}+1</Kbd><span className="mx-0.5 text-xs text-text-tertiary">...</span><Kbd>{mod}+9</Kbd></>}
                description="Jump to session (MRU order)"
              />
              <ShortcutRow
                keys={<Kbd>{mod}+,</Kbd>}
                description="Open settings"
              />
              <ShortcutRow
                keys={<Kbd>Alt+N</Kbd>}
                description="New terminal session"
              />
              <ShortcutRow
                keys={<Kbd>{mod}+B</Kbd>}
                description="Toggle sidebar"
              />
              <ShortcutRow
                keys={<Kbd>Alt+V</Kbd>}
                description="Clipboard history"
              />
              <ShortcutRow
                keys={<Kbd>?</Kbd>}
                description="Show this help"
              />
            </div>
          </div>

          {/* Command Palette Navigation */}
          <div>
            <h4 className="mb-2 text-xs font-medium text-text-tertiary">
              Command Palette Navigation
            </h4>
            <div className="space-y-0.5">
              <ShortcutRow
                keys={<><Kbd>Up</Kbd><Kbd>Down</Kbd></>}
                description="Navigate items"
              />
              <ShortcutRow
                keys={<Kbd>Enter</Kbd>}
                description="Select highlighted item"
              />
              <ShortcutRow
                keys={<><Kbd>Tab</Kbd><span className="text-xs text-text-tertiary">/</span><Kbd>Right</Kbd></>}
                description="Drill down into item"
              />
              <ShortcutRow
                keys={<><Kbd>Shift+Tab</Kbd><span className="text-xs text-text-tertiary">/</span><Kbd>Left</Kbd></>}
                description="Go back one level"
              />
              <ShortcutRow
                keys={<Kbd>Backspace</Kbd>}
                description="Go back (when query is empty)"
              />
              <ShortcutRow
                keys={<Kbd>Esc</Kbd>}
                description="Close palette"
              />
            </div>
          </div>
        </section>

        {/* Divider */}
        <div className="mb-6 border-t border-border" />

        {/* Section 2: Command Palette Guide */}
        <section>
          <h3 className="mb-3 text-sm font-semibold text-text-primary">
            Command Palette Guide
          </h3>

          <p className="mb-3 text-sm text-text-secondary">
            The command palette (<Kbd>{mod}+K</Kbd>) provides quick access to
            actions, navigation, and session management. Start typing to filter
            results or use the drill-down navigation to explore context levels.
          </p>

          <p className="mb-4 text-sm text-text-secondary">
            <span className="font-medium text-text-primary">Drill-Down Navigation:</span>{" "}
            Items with a chevron indicator support drill-down. Press{" "}
            <Kbd>Tab</Kbd> or <Kbd>Right</Kbd> to enter a deeper context, and{" "}
            <Kbd>Shift+Tab</Kbd> or <Kbd>Left</Kbd> to go back. Breadcrumbs at
            the top show your current path.
          </p>

          {/* Context Levels */}
          <h4 className="mb-2 text-xs font-medium text-text-tertiary">
            Context Levels
          </h4>
          <div className="grid grid-cols-2 gap-2">
            <ContextCard
              icon={Globe}
              title="Global"
              description="Top-level view across all hosts"
              actions={["Search transcripts", "Analytics", "Settings", "Hosts"]}
            />
            <ContextCard
              icon={Laptop}
              title="Host"
              description="Actions scoped to a single machine"
              actions={["New session", "Projects", "Sessions", "Add project"]}
            />
            <ContextCard
              icon={FolderGit2}
              title="Project"
              description="Project-level operations"
              actions={["Worktrees", "Sessions", "Start Claude", "Custom actions"]}
            />
            <ContextCard
              icon={GitBranch}
              title="Worktree"
              description="Git worktree within a project"
              actions={["Sessions", "Start Claude", "Open project", "Delete"]}
            />
            <ContextCard
              icon={Terminal}
              title="Session"
              description="Terminal session context"
              actions={["Loops", "Sibling sessions", "Close session"]}
            />
            <ContextCard
              icon={Brain}
              title="Loop"
              description="Agentic loop within a session"
              actions={["View transcript", "Metrics", "Tool calls"]}
            />
          </div>

          <p className="mt-3 text-xs text-text-tertiary">
            In local mode, the global level automatically resolves to host
            level since there is only one machine.
          </p>
        </section>
      </div>
    </div>
  );
}
