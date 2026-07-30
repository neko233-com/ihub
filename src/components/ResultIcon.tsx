import {
  AppWindow,
  FileText,
  Folder,
  Puzzle,
  Terminal,
  type LucideIcon,
} from "lucide-react";
import { safeNativeIconSrc } from "../lib/native-icons";
import type { SearchKind } from "../lib/types";

const icons: Partial<Record<SearchKind, LucideIcon>> = {
  application: AppWindow,
  file: FileText,
  folder: Folder,
  plugin: Puzzle,
  command: Terminal,
};

interface ResultIconProps {
  kind: SearchKind;
  iconSrc?: string;
  /** Reserve a neutral placeholder while a host-native icon request is pending. */
  nativeIconPending?: boolean;
}

export function ResultIcon({ kind, iconSrc, nativeIconPending = false }: ResultIconProps) {
  const nativeIconSrc = safeNativeIconSrc(iconSrc);
  const Icon = icons[kind];
  const awaitingNativeIcon = !nativeIconSrc && nativeIconPending;
  return (
    <span
      aria-hidden="true"
      className={`result-icon result-icon--${kind}${nativeIconSrc ? " is-native" : ""}${awaitingNativeIcon ? " is-loading-native" : ""}`}
    >
      {nativeIconSrc
        ? <img alt="" src={nativeIconSrc} />
        : awaitingNativeIcon || !Icon
          ? null
          : <Icon size={17} strokeWidth={1.75} />}
    </span>
  );
}
