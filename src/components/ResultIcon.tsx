import {
  AppWindow,
  FileText,
  Folder,
  Puzzle,
  Terminal,
  type LucideIcon,
} from "lucide-react";
import type { SearchKind } from "../lib/types";

const icons: Record<SearchKind, LucideIcon> = {
  file: FileText,
  folder: Folder,
  application: AppWindow,
  plugin: Puzzle,
  command: Terminal,
};

interface ResultIconProps {
  kind: SearchKind;
}

export function ResultIcon({ kind }: ResultIconProps) {
  const Icon = icons[kind];
  return (
    <span className={"result-icon result-icon--" + kind}>
      <Icon aria-hidden="true" size={17} strokeWidth={1.75} />
    </span>
  );
}
