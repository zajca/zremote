import type { PaneInfo } from "../types/terminal";

interface PaneTabBarProps {
  panes: PaneInfo[];
  activePaneId: string | undefined;
  onSelectPane: (paneId: string | undefined) => void;
}

export function PaneTabBar({
  panes,
  activePaneId,
  onSelectPane,
}: PaneTabBarProps) {
  if (panes.length === 0) return null;

  return (
    <div className="flex items-center gap-1 border-b border-border px-3 py-1">
      <button
        className={`rounded-md px-3 py-1.5 text-sm transition-colors duration-150 ${
          activePaneId === undefined
            ? "bg-bg-tertiary text-text-primary"
            : "text-text-secondary hover:text-text-primary"
        }`}
        onClick={() => onSelectPane(undefined)}
      >
        Shell
      </button>
      {panes.map((pane) => (
        <button
          key={pane.pane_id}
          className={`rounded-md px-3 py-1.5 text-sm transition-colors duration-150 ${
            activePaneId === pane.pane_id
              ? "bg-bg-tertiary text-text-primary"
              : "text-text-secondary hover:text-text-primary"
          }`}
          onClick={() => onSelectPane(pane.pane_id)}
          title={pane.pane_id}
        >
          Pane {pane.index}
        </button>
      ))}
    </div>
  );
}
