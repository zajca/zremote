import { ChevronRight } from "lucide-react";
import { Command } from "cmdk";
import type { PaletteAction } from "./types";

interface CommandPaletteItemProps {
  action: PaletteAction;
}

export function CommandPaletteItem({ action }: CommandPaletteItemProps) {
  const Icon = action.icon;

  return (
    <Command.Item
      value={[action.label, ...(action.keywords ?? [])].join(" ")}
      onSelect={action.onSelect}
      className="flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors duration-75 data-[selected=true]:bg-bg-hover data-[selected=true]:text-text-primary"
    >
      <Icon
        size={14}
        className={action.dangerous ? "text-red-400" : "text-text-tertiary"}
      />
      <span className={action.dangerous ? "text-red-400" : "text-text-secondary"}>
        {action.label}
      </span>
      {action.drillDown && (
        <ChevronRight size={12} className="ml-auto text-text-tertiary" />
      )}
    </Command.Item>
  );
}
