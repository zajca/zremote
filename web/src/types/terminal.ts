export interface PaneInfo {
  pane_id: string;
  index: number;
}

export type PaneEvent =
  | { type: "pane_added"; pane_id: string; index: number }
  | { type: "pane_removed"; pane_id: string };
