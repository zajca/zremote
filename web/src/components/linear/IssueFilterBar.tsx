import type { IssuePreset } from "../../types/linear";

interface IssueFilterBarProps {
  activePreset: IssuePreset | null;
  onPresetChange: (preset: IssuePreset | null) => void;
}

const PRESETS: { value: IssuePreset | null; label: string }[] = [
  { value: "my_issues", label: "My Issues" },
  { value: "current_sprint", label: "Sprint" },
  { value: "backlog", label: "Backlog" },
  { value: null, label: "All" },
];

export function IssueFilterBar({
  activePreset,
  onPresetChange,
}: IssueFilterBarProps) {
  return (
    <div className="flex gap-4 border-b border-border">
      {PRESETS.map((p) => (
        <button
          key={p.label}
          onClick={() => onPresetChange(p.value)}
          className={`border-b-2 px-1 pb-2 text-sm transition-colors duration-150 ${
            activePreset === p.value
              ? "border-accent text-text-primary"
              : "border-transparent text-text-tertiary hover:text-text-secondary"
          }`}
        >
          {p.label}
        </button>
      ))}
    </div>
  );
}
