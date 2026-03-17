import type { LinearIssue } from "../../types/linear";

interface IssueRowProps {
  issue: LinearIssue;
  isSelected: boolean;
  onClick: () => void;
}

const PRIORITY_INDICATORS: Record<number, { color: string; label: string }> = {
  0: { color: "text-text-tertiary", label: "None" },
  1: { color: "text-status-error", label: "Urgent" },
  2: { color: "text-status-warning", label: "High" },
  3: { color: "text-accent", label: "Medium" },
  4: { color: "text-text-secondary", label: "Low" },
};

export function IssueRow({ issue, isSelected, onClick }: IssueRowProps) {
  const priority = PRIORITY_INDICATORS[issue.priority] ?? { color: "text-text-tertiary", label: "None" };

  return (
    <button
      onClick={onClick}
      className={`flex w-full items-center gap-3 rounded-md px-3 py-2 text-left transition-colors duration-150 ${
        isSelected ? "bg-bg-active" : "hover:bg-bg-hover"
      }`}
    >
      <span className="w-20 shrink-0 font-mono text-xs text-accent">
        {issue.identifier}
      </span>
      <span className="min-w-0 flex-1 truncate text-sm text-text-primary">
        {issue.title}
      </span>
      <span className="flex items-center gap-1.5 text-xs text-text-secondary">
        <span
          className="inline-block h-2 w-2 rounded-full"
          style={{ backgroundColor: issue.state.color }}
          title={issue.state.name}
        />
        <span className="hidden sm:inline">{issue.state.name}</span>
      </span>
      {issue.assignee && (
        <span className="hidden text-xs text-text-tertiary md:inline">
          {issue.assignee.displayName || issue.assignee.name}
        </span>
      )}
      <span className={`text-xs ${priority.color}`} title={priority.label}>
        {priority.label}
      </span>
      {issue.labels.nodes.length > 0 && (
        <div className="hidden items-center gap-1 lg:flex">
          {issue.labels.nodes.slice(0, 2).map((label) => (
            <span
              key={label.id}
              className="rounded-full px-1.5 py-0.5 text-[10px]"
              style={{
                backgroundColor: `${label.color}20`,
                color: label.color,
              }}
            >
              {label.name}
            </span>
          ))}
        </div>
      )}
    </button>
  );
}
