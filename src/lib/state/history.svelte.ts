import { invoke } from "@tauri-apps/api/core";

import { formatErrorDisplay, type ErrorDisplay } from "$lib/errors";
import type {
  HistoryEntry,
  MeetingExportFormat,
  MeetingSession,
} from "$lib/types";
import { meeting } from "$lib/state/meeting-sessions.svelte";

// Page size for the history view. Hard-cap on the Rust side is 500;
// 25 is plenty per page for a dictation history that grows linearly
// with the user's actual usage (handful per day).
const HISTORY_PAGE_SIZE = 25;

/// Filter chip values for the unified History feed (#357 phase 2).
/// "all" interleaves both kinds of rows by recency; "dictation"
/// and "meetings" scope to a single kind.
export type HistoryFilter = "all" | "dictation" | "meetings";

export type FeedRow =
  | { kind: "dictation"; sortKey: number; entry: HistoryEntry }
  | { kind: "meeting"; sortKey: number; session: MeetingSession };

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

// User-selected filter chip. Defaults to "all" so the unified
// surface lands on first paint. Kept in the state module so
// the active filter survives panel unmounts and is accessible
// to `exportBundle` without the panel needing to pass it down.
let historyFilter = $state<HistoryFilter>("all");

let effectiveFilter = $derived<HistoryFilter>(historyFilter);

// Debounce handle for setSearchQuery(). 200 ms is the empirical
// sweet spot — fast enough to feel live, slow enough not to spam
// SQLite on every keystroke.
let _searchDebounceTimer: ReturnType<typeof setTimeout> | null = null;

// Merged feed — two-pointer O(N) merge of the newest-first dictation
// + meeting streams. Lives here rather than in HistoryPanel so
// `exportBundle` can pass `effectiveFilter` without a prop dance.
let mergedFeed = $derived<FeedRow[]>(
  (() => {
    const includeDictation =
      effectiveFilter === "all" || effectiveFilter === "dictation";
    const includeMeetings =
      effectiveFilter === "all" || effectiveFilter === "meetings";

    // Fast path: only one stream active — map directly, no merge.
    if (!includeMeetings) {
      if (!includeDictation) return [];
      return historyEntries.map((entry) => ({
        kind: "dictation" as const,
        sortKey: Date.parse(entry.createdAt) || 0,
        entry,
      }));
    }
    if (!includeDictation) {
      return meeting.sessions.map((session) => ({
        kind: "meeting" as const,
        sortKey: Date.parse(session.startedAt) || 0,
        session,
      }));
    }

    // Both streams active. Both arrive newest-first from the backend,
    // so a two-pointer merge produces a sorted result in O(N).
    const d: FeedRow[] = historyEntries.map((entry) => ({
      kind: "dictation" as const,
      sortKey: Date.parse(entry.createdAt) || 0,
      entry,
    }));
    const m: FeedRow[] = meeting.sessions.map((session) => ({
      kind: "meeting" as const,
      sortKey: Date.parse(session.startedAt) || 0,
      session,
    }));

    const out: FeedRow[] = [];
    let di = 0,
      mi = 0;
    while (di < d.length && mi < m.length) {
      if (d[di].sortKey >= m[mi].sortKey) {
        out.push(d[di++]);
      } else {
        out.push(m[mi++]);
      }
    }
    while (di < d.length) out.push(d[di++]);
    while (mi < m.length) out.push(m[mi++]);
    return out;
  })(),
);

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
  get filter() {
    return historyFilter;
  },
  set filter(val: HistoryFilter) {
    historyFilter = val;
  },
  get effectiveFilter() {
    return effectiveFilter;
  },
  get mergedFeed() {
    return mergedFeed;
  },
  // True once both dictation and meeting data have completed their first load.
  // HistoryPanel gates its empty-state and skeleton on this single flag.
  get feedLoaded() {
    return historyLoaded && meeting.sessionsLoaded;
  },
  // Refresh both dictation history and meeting sessions in parallel.
  // Use this whenever a search-query change or post-recording invalidation
  // needs to keep the unified feed in sync — avoids scattering paired
  // history.refresh() + meeting.refresh() calls across consumers.
  async feedRefresh() {
    await Promise.all([history.refresh(), meeting.refresh()]);
  },
  /** Update the search query and debounce a feedRefresh by 200 ms.
   *  Replaces the inline timer that used to live in +page.svelte. */
  setSearchQuery(query: string) {
    historyQuery = query;
    if (_searchDebounceTimer !== null) clearTimeout(_searchDebounceTimer);
    _searchDebounceTimer = setTimeout(() => {
      void history.feedRefresh();
    }, 200);
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
  }) {
    try {
      const resolvedKind: "both" | "dictation" | "meetings" =
        args.kind === "auto"
          ? effectiveFilter === "dictation"
            ? "dictation"
            : effectiveFilter === "meetings"
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
