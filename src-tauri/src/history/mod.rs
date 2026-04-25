// History module — SQLite-backed transcription log.
//
// Responsibilities:
//   - Insert a new history entry after each successful transcription.
//   - Paginated list query with optional full-text search.
//   - Copy-to-clipboard and delete commands.
//   - CSV export for the export-all action.
//   - Store foreground app name and window title per entry (populated by the `ipc` layer).

// TODO(#5): implement history CRUD queries on top of the db layer
