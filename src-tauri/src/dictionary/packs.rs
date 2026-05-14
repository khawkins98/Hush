//! Bundled vocabulary/replacement preset packs.
//!
//! Pack definitions are **static** — compiled into the binary. No file I/O,
//! no network. The user's choice of which packs are active is persisted as
//! a JSON array of slugs in the settings table
//! (`settings::keys::ENABLED_PACKS`). Enabling/disabling a pack updates
//! that settings row; the pack contents are never materialised into the
//! `dictionary_terms` or `replacements` tables.
//!
//! ## Why static rather than database-materialised
//!
//! Materialising pack rows into user tables creates an ownership/edit
//! ambiguity (did the user edit this row or was it inserted by a pack?),
//! makes disable destructive (deletes rows the user may have customised),
//! and silently keeps stale pack definitions on update. Persisting only
//! enabled slugs and compositing at runtime avoids all of those problems.
//!
//! ## How packs are applied at runtime
//!
//! `dictation/pipeline.rs::load_initial_prompt` reads enabled pack slugs
//! from settings, fetches their vocabulary terms, deduplicates against the
//! user's personal vocabulary, and combines everything into the Whisper
//! initial prompt. `load_replacement_rules` similarly appends pack rules
//! (sorted before user rules so user rules can override them).

use serde::Serialize;

/// A bundled preset pack.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackDescriptor {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Vocabulary terms to add to the Whisper initial prompt.
    pub vocabulary: &'static [&'static str],
    /// Post-transcription replacement rules: `(find_text, replace_text)`.
    pub replacements: &'static [(&'static str, &'static str)],
}

/// All built-in packs, in display order.
pub fn all_packs() -> &'static [PackDescriptor] {
    PACKS
}

/// Find a pack by slug. `None` if the slug is not in [`all_packs`].
pub fn find_pack(slug: &str) -> Option<&'static PackDescriptor> {
    PACKS.iter().find(|p| p.slug == slug)
}

// ---------------------------------------------------------------------------
// Pack definitions
// ---------------------------------------------------------------------------

static PACKS: &[PackDescriptor] = &[DEV_GENERAL, BUSINESS];

static DEV_GENERAL: PackDescriptor = PackDescriptor {
    slug: "dev-general",
    name: "Developer — General",
    description: "Common software-development terms, platform names, and Whisper mishearing \
                  corrections for everyday coding conversations.",
    vocabulary: &[
        // Platform + tool names Whisper commonly mishears
        "GitHub",
        "GitLab",
        "Bitbucket",
        "Jira",
        "Confluence",
        "Slack",
        "Notion",
        "Linear",
        "Figma",
        "Vercel",
        "Netlify",
        "Heroku",
        "Docker",
        "Kubernetes",
        "Terraform",
        "Ansible",
        "Homebrew",
        "npm",
        "pnpm",
        "Yarn",
        "Cargo",
        "Rust",
        "TypeScript",
        "JavaScript",
        "Python",
        "PostgreSQL",
        "SQLite",
        "Redis",
        "GraphQL",
        "REST",
        "API",
        "JSON",
        "YAML",
        "TOML",
        "Markdown",
        // Workflow / process terms
        "PR",
        "repo",
        "CI",
        "CD",
        "DevOps",
        "async",
        "await",
        "refactor",
        "deploy",
        "rollback",
        "hotfix",
        "codebase",
        "monorepo",
    ],
    replacements: &[
        // Common Whisper mishearings for developer vocabulary
        ("get hub", "GitHub"),
        ("get hub's", "GitHub's"),
        ("git hub", "GitHub"),
        ("get lab", "GitLab"),
        ("docker compose", "Docker Compose"),
        ("docker file", "Dockerfile"),
        ("terra form", "Terraform"),
        ("kube ctl", "kubectl"),
        ("cube ctl", "kubectl"),
        ("node j.s.", "Node.js"),
        ("node.js.", "Node.js"),
        ("type script", "TypeScript"),
        ("java script", "JavaScript"),
        ("post gres", "PostgreSQL"),
        ("my sequel", "MySQL"),
        ("s q lite", "SQLite"),
        ("jason", "JSON"),
    ],
};

static BUSINESS: PackDescriptor = PackDescriptor {
    slug: "business",
    name: "Business",
    description: "Meeting-room language, business metrics, and common corrections for \
                  workplace dictation.",
    vocabulary: &[
        "KPI",
        "OKR",
        "ROI",
        "ARR",
        "MRR",
        "P&L",
        "Q1",
        "Q2",
        "Q3",
        "Q4",
        "CEO",
        "CTO",
        "CFO",
        "COO",
        "VP",
        "stakeholder",
        "deliverable",
        "milestone",
        "roadmap",
        "backlog",
        "sprint",
        "scrum",
        "agile",
        "bandwidth",
        "synergy",
        "scalable",
        "onboarding",
        "offboarding",
        "onsite",
        "offsite",
        "async",
    ],
    replacements: &[
        // Common phrase mishearsings in business speech
        ("deck", "slide deck"),
        ("take it offline", "take it offline"),
        ("circle back", "circle back"),
        ("key pee eye", "KPI"),
        ("o.k.r.", "OKR"),
        ("o k r", "OKR"),
        ("r.o.i.", "ROI"),
    ],
};
