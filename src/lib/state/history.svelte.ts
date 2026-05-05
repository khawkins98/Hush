import { invoke } from "@tauri-apps/api/core";

import { formatErrorDisplay, type ErrorDisplay } from "$lib/errors";
import type {
  HistoryEntry,
  MeetingExportFormat,
  MeetingSession,
} from "$lib/types";

// Page size for the history view. Hard-cap on the Rust side is 500;
// 25 is plenty per page for a dictation history that grows linearly
// with the user's actual usage (handful per day).
const HISTORY_PAGE_SIZE = 25;

let historyEntries = $state<HistoryEntry[]>([]);
let historyLoaded = $state(false);
let historyQuery = $state("");
let historySearching = $state(false);
let historyError = $state<ErrorDisplay | null>(null);
// Unfiltered total — `historyEntries` shows the current page /
// filtered slice, so the total drives the sidebar counter and
// the "Clear all N" confirmation copy.
let historyTotalCount = $state(0);
// Sentinel that any history-touching command bumps so consumers can
// react to an external invalidation.
let historyVersion = $state(0);

export const history = {
  get entries() {
    return historyEntries;
  },
  set entries(val: HistoryEntry[]) {
    historyEntries = val;
  },
  get loaded() {
    return historyLoaded;
  },
  set loaded(val: boolean) {
    historyLoaded = val;
  },
  get historyQuery() {
    return historyQuery;
  },
  set historyQuery(val: string) {
    historyQuery = val;
  },
  get searching() {
    return historySearching;
  },
  set searching(val: boolean) {
    historySearching = val;
  },
  get error() {
    return historyError;
  },
  set error(val: ErrorDisplay | null) {
    historyError = val;
  },
  get totalCount() {
    return historyTotalCount;
  },
  set totalCount(val: number) {
    historyTotalCount = val;
  },
  get version() {
    return historyVersion;
  },
  set version(val: number) {
    historyVersion = val;
  },
  async refresh() {
    historyError = null;
    historySearching = true;
    try {
      const [entries, total] = await Promise.all([
        invoke<HistoryEntry[]>("history_search", {
          query: historyQuery,
          limit: HISTORY_PAGE_SIZE,
          offset: 0,
        }),
        invoke<number>("history_count"),
      ]);
      historyEntries = entries;
      historyTotalCount = total;
      historyVersion += 1;
    } catch (e) {
      historyError = formatErrorDisplay(e);
    } finally {
      historyLoaded = true;
      historySearching = false;
    }
  },
  async copyEntry(entry: HistoryEntry) {
    try {
      await navigator.clipboard.writeText(entry.transcript);
    } catch (e) {
      historyError = {
        headline: "Copy failed",
        hint: "Hush couldn't write to the clipboard. Try copying again, or paste from this entry's text directly.",
        details: String(e),
      };
    }
  },
  async deleteEntry(entry: HistoryEntry) {
    try {
      await invoke("history_delete", { id: entry.id });
      historyEntries = historyEntries.filter((e) => e.id !== entry.id);
      void history.refresh();
    } catch (e) {
      historyError = formatErrorDisplay(e);
    }
  },
  async exportDictationCsv(entry: HistoryEntry) {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const datePart = entry.createdAt.slice(0, 10);
      const path = await save({
        defaultPath: `hush-dictation-${datePart}.csv`,
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
      if (path === null) {
        return;
      }
      await invoke("history_export_row_csv", { id: entry.id, path });
    } catch (e) {
      historyError = formatErrorDisplay(e);
    }
  },
  async exportMeetingSession(
    session: MeetingSession,
    format: MeetingExportFormat,
  ) {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const datePart = session.startedAt.slice(0, 10);
      const ext = format === "text" ? "txt" : format;
      const filterName =
        format === "text" ? "Plain text" : format === "csv" ? "CSV" : "JSON";
      const path = await save({
        defaultPath: `hush-meeting-${datePart}.${ext}`,
        filters: [{ name: filterName, extensions: [ext] }],
      });
      if (path === null) {
        return;
      }
      await invoke("meeting_session_export", {
        id: session.id,
        format,
        path,
      });
    } catch (e) {
      historyError = formatErrorDisplay(e);
    }
  },
  async exportBundle(args: {
    kind: "auto" | "dictation" | "meetings" | "both";
    meetingFormat: MeetingExportFormat;
    activeFilter: "all" | "dictation" | "meetings";
  }) {
    try {
      const resolvedKind: "both" | "dictation" | "meetings" =
        args.kind === "auto"
          ? args.activeFilter === "dictation"
            ? "dictation"
            : args.activeFilter === "meetings"
              ? "meetings"
              : "both"
          : args.kind;

      const { open } = await import("@tauri-apps/plugin-dialog");
      const directory = await open({
        directory: true,
        multiple: false,
        title: "Export filtered to…",
      });
      if (directory === null || Array.isArray(directory)) {
        return;
      }
      const result = await invoke<{ directory: string; written: number }>(
        "history_export_bundle",
        {
          options: {
            query: historyQuery,
            kind: resolvedKind,
            meetingFormat: args.meetingFormat,
          },
          directory,
        },
      );
      historyError = {
        headline:
          result.written === 0
            ? "No rows matched the current filter."
            : `Wrote ${result.written} file${result.written === 1 ? "" : "s"} to ${result.directory}.`,
        hint: undefined,
        details: undefined,
      };
    } catch (e) {
      historyError = formatErrorDisplay(e);
    }
  },
  async clearAll() {
    try {
      const removed = await invoke<number>("history_clear");
      historyEntries = [];
      historyTotalCount = 0;
      historyVersion += 1;
      historyError = null;
      void removed;
    } catch (e) {
      historyError = formatErrorDisplay(e);
    }
  },
};
