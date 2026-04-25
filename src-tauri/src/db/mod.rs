// Database module — SQLite connection pool and migrations via `sqlx`.
//
// Responsibilities:
//   - Initialise a connection pool pointing at the platform app-data directory.
//   - Run embedded migrations on startup (`sqlx::migrate!()`).
//   - Re-export the pool type for use by history, dictionary, and settings modules.
//
// Schema lives in `src-tauri/migrations/`. Tables: history, dictionary_terms, replacements, settings.

// TODO(#6): initialise sqlx pool, embed and run migrations
