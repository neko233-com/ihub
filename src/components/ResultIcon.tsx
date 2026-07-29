import {
  FileText,
  Folder,
  Puzzle,
  Terminal,
  type LucideIcon,
} from "lucide-react";
import { safeNativeIconSrc } from "../lib/native-icons";
import type { SearchKind } from "../lib/types";

const icons: Partial<Record<SearchKind, LucideIcon>> = {
  file: FileText,
  folder: Folder,
  plugin: Puzzle,
  command: Terminal,
};

interface ResultIconProps {
  kind: SearchKind;
  iconSrc?: string;
}

export function ResultIcon({ kind, iconSrc }: ResultIconProps) {
  const nativeIconSrc = safeNativeIconSrc(iconSrc);
  const Icon = icons[kind];
  const nativeIconPending = kind === "application" && !nativeIconSrc;
  return (
    <span
      aria-hidden="true"
      className={`result-icon result-icon--${kind}${nativeIconSrc ? " is-native" : ""}${nativeIconPending ? " is-loading-native" : ""}`}
    >
      {nativeIconSrc
        ? <img alt="" src={nativeIconSrc} />
        : nativeIconPending || !Icon
          ? null
          : <Icon size={17} strokeWidth={1.75} />}
    </span>
  );
}
