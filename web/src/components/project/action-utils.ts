import * as LucideIcons from "lucide-react";
import { Terminal } from "lucide-react";
import type { ActionScope, ProjectAction } from "../../lib/api";

const TEMPLATE_VAR_RE = /\{\{(worktree_path|branch|worktree_name)\}\}/g;

export function effectiveScopes(action: ProjectAction): ActionScope[] {
  if (action.scopes && action.scopes.length > 0) return action.scopes;
  return action.worktree_scoped
    ? ["worktree", "command_palette"]
    : ["project", "command_palette"];
}

export function hasScope(action: ProjectAction, scope: ActionScope): boolean {
  return effectiveScopes(action).includes(scope);
}

export function detectMissingInputs(
  command: string,
  workingDir: string | undefined,
  worktreePath: string | undefined,
  branch: string | undefined,
): { needsWorktree: boolean; needsBranch: boolean } {
  const vars = new Set<string>();
  for (const m of command.matchAll(TEMPLATE_VAR_RE)) if (m[1]) vars.add(m[1]);
  if (workingDir) {
    for (const m of workingDir.matchAll(TEMPLATE_VAR_RE)) if (m[1]) vars.add(m[1]);
  }

  const needsWorktree = !worktreePath && (vars.has("worktree_path") || vars.has("worktree_name"));
  const needsBranch = !branch && !needsWorktree && vars.has("branch");
  return { needsWorktree, needsBranch };
}

export function getActionIcon(name?: string): React.ComponentType<LucideIcons.LucideProps> {
  if (!name) return Terminal;
  const pascalName = name
    .split("-")
    .map((s) => s.charAt(0).toUpperCase() + s.slice(1))
    .join("");
  return (
    ((LucideIcons as Record<string, unknown>)[pascalName] as React.ComponentType<LucideIcons.LucideProps>) ||
    Terminal
  );
}
