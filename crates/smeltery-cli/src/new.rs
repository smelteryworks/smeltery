//! `smeltery new`: create an app from the embedded templates.

use std::fmt;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};

use crate::frontend::{self, NodeCheck};
use crate::make::stamp;
use crate::templates::{self, TEMPLATES, When};
use crate::ui::{Badge, Ui};

/// Arguments of `smeltery new`.
#[derive(Debug, Args)]
pub(crate) struct NewArgs {
    /// App name: lowercase letters, digits, `-` and `_`, starting with a letter.
    name: String,
    /// What the app is: a web app, or a headless app (Watchfire agents, jobs and the scheduler, no web pages).
    #[arg(long, value_enum)]
    kind: Option<Kind>,
    /// Database backend.
    #[arg(long, value_enum)]
    db: Option<Db>,
    /// Starter kit (web apps): Mold + Sparks, or React / Vue (Inertia, TypeScript).
    #[arg(long, value_enum)]
    frontend: Option<Frontend>,
    /// AI-agent support files, comma separated.
    #[arg(long, value_enum, value_delimiter = ',')]
    bellows: Option<Vec<BellowsArg>>,
    /// Tailwind CSS (web apps; default): with Mold, download the pinned standalone binary into the per-user
    /// Smeltery folder; with React / Vue, the `tailwindcss` npm packages and Vite plugin.
    #[arg(long, overrides_with = "no_tailwind")]
    tailwind: bool,
    /// No Tailwind CSS: the app keeps its prebuilt CSS.
    #[arg(long)]
    no_tailwind: bool,
    /// Add Alpine.js (the release embedded in the CLI) to `public/assets/js/` and the layout (Mold frontend).
    #[arg(long, overrides_with = "no_alpine")]
    alpine: bool,
    /// Do not add Alpine.js (default).
    #[arg(long)]
    no_alpine: bool,
    /// Building blocks to smelt into a web app, comma separated (default: watchfire,temper). Headless apps always
    /// have Watchfire.
    #[arg(long, value_enum, value_delimiter = ',')]
    smelt: Option<Vec<SmeltArg>>,
    /// Run `npm install` in the new app (React / Vue; default, when Node.js 20.19+ or 22.12+ is found).
    #[arg(long, overrides_with = "no_npm")]
    npm: bool,
    /// Do not run `npm install`.
    #[arg(long)]
    no_npm: bool,
    /// Run migrations after creating the app (default).
    #[arg(long, overrides_with = "no_migrate")]
    migrate: bool,
    /// Do not run migrations.
    #[arg(long)]
    no_migrate: bool,
    /// Run seeders after creating the app (default).
    #[arg(long, overrides_with = "no_seed")]
    seed: bool,
    /// Do not run seeders.
    #[arg(long)]
    no_seed: bool,
    /// Initialize a git repository (default).
    #[arg(long, overrides_with = "no_git")]
    git: bool,
    /// Do not initialize a git repository.
    #[arg(long)]
    no_git: bool,
    /// Directory to create the app in (default: the current directory).
    #[arg(long)]
    path: Option<PathBuf>,
    /// Depend on the Smeltery crates of this local checkout by path.
    #[arg(long, hide = true)]
    smeltery_path: Option<PathBuf>,
}

impl NewArgs {
    fn any_question_flag(&self) -> bool {
        self.kind.is_some()
            || self.db.is_some()
            || self.frontend.is_some()
            || self.bellows.is_some()
            || self.tailwind
            || self.no_tailwind
            || self.alpine
            || self.no_alpine
            || self.smelt.is_some()
            || self.npm
            || self.no_npm
            || self.migrate
            || self.no_migrate
            || self.seed
            || self.no_seed
            || self.git
            || self.no_git
    }
}

/// What the app is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Kind {
    /// Web pages, with the building blocks chosen by `--smelt`.
    Web,
    /// Watchfire agents, jobs and the scheduler, no web pages.
    Headless,
}

impl Kind {
    /// The "What are you building?" answers, in the order of the prompt.
    pub(crate) const ALL: [Kind; 2] = [Kind::Web, Kind::Headless];

    /// The answer shown in the prompt.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Kind::Web => "Web app",
            Kind::Headless => "Headless (agents, jobs and the scheduler, no web pages)",
        }
    }

    pub(crate) fn has_web(self) -> bool {
        self == Kind::Web
    }
}

/// An optional first-party building block of a web app (the "Building blocks" question and `--smelt`).
///
/// Adding a block is one entry in each of: this enum, the table [`Block::spec`], [`SmeltArg`] and [`Blocks`]; the
/// matches are exhaustive, so the compiler points at every place. Headless apps take no block (they always run
/// Watchfire and have no pages).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Block {
    /// Watchfire: agents, jobs, the scheduler and the `/_watchfire` dashboard.
    Watchfire,
    /// Temper: login, registration, password reset, email verification, two-factor, the dashboard and the
    /// settings pages.
    Auth,
    /// Hallmark: API tokens (`routes/api.rs` issues them for an e-mail address and password); needs Temper.
    Hallmark,
    /// Anvil: WebSockets and broadcasting (`routes/channels.rs`, an example event).
    Anvil,
    /// Prospect: full-text search for models (`make:model --searchable`).
    Search,
}

/// One row of the building-block table: the block's texts and how it relates to the other blocks (D-508). The
/// checklist, `--smelt`, the notes under the checklist, the summary and the one-line command all read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockSpec {
    /// The value of `--smelt`.
    pub(crate) value: &'static str,
    /// The name shown as the answer and in the notes.
    pub(crate) name: &'static str,
    /// The checklist's line.
    pub(crate) label: &'static str,
    /// Blocks it cannot work without: chosen with it, and said on screen (also with `--smelt`).
    pub(crate) requires: &'static [Block],
    /// Blocks it works well with: a hint when this block is chosen and they are not; never chosen for the user.
    pub(crate) recommends: &'static [Recommendation],
}

/// A block another block works well with, and what the pair gives (the hint's text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Recommendation {
    pub(crate) block: Block,
    /// What the pair gives, e.g. `private channels`.
    pub(crate) gives: &'static str,
}

impl Block {
    /// Every block, in the order of the prompt and of `--smelt`.
    pub(crate) const ALL: [Block; 5] = [
        Block::Watchfire,
        Block::Auth,
        Block::Hallmark,
        Block::Anvil,
        Block::Search,
    ];

    /// The building-block table: one row per block.
    pub(crate) const fn spec(self) -> BlockSpec {
        match self {
            Block::Watchfire => BlockSpec {
                value: "watchfire",
                name: "Watchfire",
                label: "Watchfire: agents, jobs, the scheduler and the /_watchfire dashboard",
                requires: &[],
                recommends: &[],
            },
            Block::Auth => BlockSpec {
                value: "temper",
                name: "Temper",
                label: "Temper: login, registration, password reset, email verification, two-factor",
                requires: &[],
                recommends: &[],
            },
            // API tokens belong to user accounts (D-467, HALLMARK.md H-A4).
            Block::Hallmark => BlockSpec {
                value: "hallmark",
                name: "Hallmark",
                label: "Hallmark: API tokens for mobile apps, desktop apps and other clients",
                requires: &[Block::Auth],
                recommends: &[],
            },
            Block::Anvil => BlockSpec {
                value: "anvil",
                name: "Anvil",
                label: "Anvil: WebSockets and broadcasting (the Pusher protocol), channels and events",
                requires: &[],
                recommends: &[
                    Recommendation {
                        block: Block::Auth,
                        gives: "private channels for signed-in users",
                    },
                    Recommendation {
                        block: Block::Hallmark,
                        gives: "private channels for mobile apps and other clients",
                    },
                    Recommendation {
                        block: Block::Watchfire,
                        gives: "broadcasting from agents and jobs",
                    },
                ],
            },
            // No recommendation (D-539): the database driver keeps its index current itself, so Watchfire adds
            // nothing to search today; the row gains it with the external engines that sync through jobs.
            Block::Search => BlockSpec {
                value: "prospect",
                name: "Prospect",
                label: "Prospect: full-text search for models",
                requires: &[],
                recommends: &[],
            },
        }
    }

    /// The value of `--smelt`.
    pub(crate) fn value(self) -> &'static str {
        self.spec().value
    }

    /// The name shown as the answer.
    pub(crate) fn name(self) -> &'static str {
        self.spec().name
    }

    /// The prompt's line for the block (every starter kit writes what it names).
    pub(crate) fn label(self) -> &'static str {
        self.spec().label
    }
}

/// One value of `--smelt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SmeltArg {
    /// No building blocks.
    None,
    /// Watchfire agents, jobs, the scheduler and the /_watchfire dashboard.
    Watchfire,
    /// Temper: login, registration, password reset, email verification, two-factor, the dashboard and the
    /// settings pages.
    #[value(name = "temper")]
    Auth,
    /// Hallmark: API tokens for mobile apps, desktop apps and other clients (adds Temper).
    Hallmark,
    /// Anvil: WebSockets and broadcasting (the Pusher protocol), channels and events.
    Anvil,
    /// Prospect: full-text search for models.
    #[value(name = "prospect")]
    Search,
}

impl SmeltArg {
    fn block(self) -> Option<Block> {
        match self {
            SmeltArg::None => None,
            SmeltArg::Watchfire => Some(Block::Watchfire),
            SmeltArg::Auth => Some(Block::Auth),
            SmeltArg::Hallmark => Some(Block::Hallmark),
            SmeltArg::Anvil => Some(Block::Anvil),
            SmeltArg::Search => Some(Block::Search),
        }
    }
}

/// The building blocks smelted into a web app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Blocks {
    pub(crate) watchfire: bool,
    pub(crate) auth: bool,
    pub(crate) hallmark: bool,
    pub(crate) anvil: bool,
    pub(crate) search: bool,
}

impl Default for Blocks {
    /// Watchfire and Temper (the prompt's preselection and the default without `--smelt`); Hallmark and
    /// Anvil are chosen on purpose (FEATURES.md, the owner's defaults).
    fn default() -> Self {
        Blocks {
            watchfire: true,
            auth: true,
            hallmark: false,
            anvil: false,
            search: false,
        }
    }
}

impl Blocks {
    /// No block.
    pub(crate) const NONE: Blocks = Blocks {
        watchfire: false,
        auth: false,
        hallmark: false,
        anvil: false,
        search: false,
    };

    pub(crate) fn has(self, block: Block) -> bool {
        match block {
            Block::Watchfire => self.watchfire,
            Block::Auth => self.auth,
            Block::Hallmark => self.hallmark,
            Block::Anvil => self.anvil,
            Block::Search => self.search,
        }
    }

    pub(crate) fn with(mut self, block: Block) -> Self {
        match block {
            Block::Watchfire => self.watchfire = true,
            Block::Auth => self.auth = true,
            Block::Hallmark => self.hallmark = true,
            Block::Anvil => self.anvil = true,
            Block::Search => self.search = true,
        }
        self
    }

    fn from_args(args: &[SmeltArg]) -> Self {
        args.iter()
            .filter_map(|a| a.block())
            .fold(Blocks::NONE, Blocks::with)
    }

    /// These blocks plus everything they require, and one line per block that was added (D-508).
    pub(crate) fn with_required(self) -> (Blocks, Vec<String>) {
        self.with_required_by(Block::spec)
    }

    /// [`Blocks::with_required`] over any table (the tests use their own rows).
    fn with_required_by(self, spec: impl Fn(Block) -> BlockSpec) -> (Blocks, Vec<String>) {
        let mut blocks = self;
        let mut notes = Vec::new();
        // A round that adds nothing ends the loop; every other round adds a block, so it runs at most
        // `Block::ALL.len() + 1` times.
        loop {
            let mut added = false;
            for block in blocks.iter().collect::<Vec<_>>() {
                for &needed in spec(block).requires {
                    if !blocks.has(needed) {
                        blocks = blocks.with(needed);
                        added = true;
                        notes.push(format!(
                            "{} needs {}: added",
                            spec(block).name,
                            spec(needed).name
                        ));
                    }
                }
            }
            if !added {
                return (blocks, notes);
            }
        }
    }

    /// One hint per chosen block and each block it works well with that is not chosen.
    pub(crate) fn hints(self) -> Vec<String> {
        self.hints_by(Block::spec)
    }

    /// [`Blocks::hints`] over any table (the tests use their own rows).
    fn hints_by(self, spec: impl Fn(Block) -> BlockSpec) -> Vec<String> {
        let mut hints = Vec::new();
        for block in self.iter() {
            let row = spec(block);
            for r in row.recommends.iter().filter(|r| !self.has(r.block)) {
                hints.push(format!(
                    "{} works with {}: {}",
                    row.name,
                    spec(r.block).name,
                    r.gives
                ));
            }
        }
        hints
    }

    /// The chosen blocks, in [`Block::ALL`] order.
    fn iter(self) -> impl Iterator<Item = Block> {
        Block::ALL.into_iter().filter(move |b| self.has(*b))
    }

    /// The prompt's entries, in [`Block::ALL`] order.
    fn labels() -> Vec<&'static str> {
        Block::ALL.iter().map(|b| b.label()).collect()
    }

    /// The indices of the prompt's entries selected at the start: the blocks of [`Blocks::default`].
    fn preselected() -> Vec<usize> {
        Block::ALL
            .iter()
            .enumerate()
            .filter(|(_, b)| Blocks::default().has(**b))
            .map(|(i, _)| i)
            .collect()
    }

    /// The blocks whose prompt entries were picked.
    fn from_labels(picked: &[&str]) -> Self {
        Block::ALL
            .into_iter()
            .filter(|b| picked.contains(&b.label()))
            .fold(Blocks::NONE, Blocks::with)
    }

    /// The answer line for the picked prompt entries and what they require (names, not their whole descriptions,
    /// so it fits one line).
    fn answer_for(picked: &[&str]) -> String {
        Blocks::from_labels(picked).with_required().0.answer()
    }

    /// The answer line of the question: the chosen blocks' names, or `none`.
    fn answer(self) -> String {
        let names: Vec<&str> = self.iter().map(Block::name).collect();
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    }
}

impl fmt::Display for Blocks {
    /// The value of `--smelt`: e.g. `watchfire,temper`, or `none`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<&str> = self.iter().map(Block::value).collect();
        if parts.is_empty() {
            f.write_str("none")
        } else {
            f.write_str(&parts.join(","))
        }
    }
}

/// Database backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Db {
    /// SQLite file in `database/`.
    Sqlite,
    /// PostgreSQL.
    Postgres,
    /// MySQL / MariaDB.
    Mysql,
}

/// Starter kit (the frontend stack).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Frontend {
    /// Mold templates with Sparks live components.
    Mold,
    /// React pages through Alloy (Inertia), TypeScript, built by Vite.
    React,
    /// Vue pages through Alloy (Inertia), TypeScript, built by Vite.
    Vue,
}

impl Frontend {
    /// The "Starter kit" answers, in the order of the prompt.
    pub(crate) const ALL: [Frontend; 3] = [Frontend::Mold, Frontend::React, Frontend::Vue];

    /// The answer shown in the prompt and the summary.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Frontend::Mold => "Mold + Sparks",
            Frontend::React => "React (Inertia, TypeScript)",
            Frontend::Vue => "Vue (Inertia, TypeScript)",
        }
    }

    /// A JavaScript kit (React or Vue through Alloy) with a `package.json`.
    pub(crate) fn is_js(self) -> bool {
        self != Frontend::Mold
    }
}

/// One value of `--bellows`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum BellowsArg {
    /// No Bellows files.
    None,
    /// `.mcp.json` for the Bellows MCP server.
    Mcp,
    /// `.bellows/skills/`.
    Skills,
    /// `.bellows/guidelines.md`.
    Guidelines,
    /// All of the above.
    All,
}

/// The chosen Bellows parts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Bellows {
    pub(crate) mcp: bool,
    pub(crate) skills: bool,
    pub(crate) guidelines: bool,
}

impl Bellows {
    fn from_args(args: &[BellowsArg]) -> Self {
        let mut b = Bellows::default();
        for arg in args {
            match arg {
                BellowsArg::None => {}
                BellowsArg::Mcp => b.mcp = true,
                BellowsArg::Skills => b.skills = true,
                BellowsArg::Guidelines => b.guidelines = true,
                BellowsArg::All => {
                    b = Bellows {
                        mcp: true,
                        skills: true,
                        guidelines: true,
                    }
                }
            }
        }
        b
    }
}

impl fmt::Display for Bellows {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<&str> = [
            (self.mcp, "mcp"),
            (self.skills, "skills"),
            (self.guidelines, "guidelines"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        match parts.len() {
            0 => f.write_str("none"),
            3 => f.write_str("all"),
            _ => f.write_str(&parts.join(",")),
        }
    }
}

/// Every answer `smeltery new` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewOptions {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) db: Db,
    /// The building blocks (web apps; headless apps ignore it: they always run Watchfire and have no pages).
    pub(crate) blocks: Blocks,
    /// `None` for headless apps.
    pub(crate) frontend: Option<Frontend>,
    pub(crate) bellows: Bellows,
    /// Install the Tailwind binary (always `false` for headless apps).
    pub(crate) tailwind: bool,
    /// Add Alpine.js (only with the Mold frontend; always `false` for headless apps).
    pub(crate) alpine: bool,
    /// Run `npm install` (only with a React / Vue kit; always `false` otherwise).
    pub(crate) npm: bool,
    pub(crate) migrate: bool,
    pub(crate) seed: bool,
    pub(crate) git: bool,
}

impl NewOptions {
    /// The defaults for `name`.
    #[cfg(test)]
    pub(crate) fn defaults(name: &str) -> Self {
        NewOptions {
            name: name.to_owned(),
            kind: Kind::Web,
            db: Db::Sqlite,
            blocks: Blocks::default(),
            frontend: Some(Frontend::Mold),
            bellows: Bellows::default(),
            tailwind: true,
            alpine: false,
            npm: false,
            migrate: true,
            seed: true,
            git: true,
        }
    }

    /// The app has a React or Vue kit.
    pub(crate) fn is_js(&self) -> bool {
        self.frontend.is_some_and(Frontend::is_js)
    }

    /// The app runs Watchfire: every headless app, and web apps with the Watchfire block.
    pub(crate) fn has_agents(&self) -> bool {
        !self.kind.has_web() || self.blocks.watchfire
    }

    /// The app has the authentication scaffolding (web apps with the Temper block).
    pub(crate) fn has_auth(&self) -> bool {
        self.kind.has_web() && self.blocks.auth
    }

    /// The app's authentication is Temper (every starter kit, D-482).
    pub(crate) fn has_temper(&self) -> bool {
        self.has_auth()
    }

    /// The app has Hallmark's API tokens (web apps with the Hallmark block, which brings Temper).
    pub(crate) fn has_hallmark(&self) -> bool {
        self.kind.has_web() && self.blocks.hallmark && self.blocks.auth
    }

    /// The app has Anvil's WebSockets and broadcasting (web apps with the Anvil block).
    pub(crate) fn has_anvil(&self) -> bool {
        self.kind.has_web() && self.blocks.anvil
    }

    /// The app has Prospect's search (web apps with the Prospect block).
    pub(crate) fn has_search(&self) -> bool {
        self.kind.has_web() && self.blocks.search
    }

    fn from_flags(args: &NewArgs) -> Self {
        let kind = args.kind.unwrap_or(Kind::Web);
        let frontend = kind
            .has_web()
            .then(|| args.frontend.unwrap_or(Frontend::Mold));
        NewOptions {
            name: args.name.clone(),
            kind,
            db: args.db.unwrap_or(Db::Sqlite),
            // Headless apps take no block (`run` refuses `--smelt` with blocks for them, `check_flags`).
            blocks: args
                .smelt
                .as_deref()
                .filter(|_| kind.has_web())
                .map(Blocks::from_args)
                .unwrap_or_default()
                .with_required()
                .0,
            frontend,
            bellows: args
                .bellows
                .as_deref()
                .map(Bellows::from_args)
                .unwrap_or_default(),
            tailwind: kind.has_web() && !args.no_tailwind,
            alpine: frontend == Some(Frontend::Mold) && args.alpine,
            npm: frontend.is_some_and(Frontend::is_js) && !args.no_npm,
            migrate: !args.no_migrate,
            seed: !args.no_seed,
            git: !args.no_git,
        }
    }

    /// The one-line command that creates the same app without questions.
    pub(crate) fn command_line(&self) -> String {
        let kind = self
            .kind
            .to_possible_value()
            .map(|v| v.get_name().to_owned())
            .unwrap_or_default();
        let db = self
            .db
            .to_possible_value()
            .map(|v| v.get_name().to_owned())
            .unwrap_or_default();
        let flag = |on: bool, name: &str| {
            if on {
                format!("--{name}")
            } else {
                format!("--no-{name}")
            }
        };
        // `--frontend`, `--tailwind` and `--smelt` only mean something for web apps, `--alpine` for the Mold
        // frontend, `--npm` for React and Vue. The flags follow the order of the questions.
        let mut web = String::new();
        if let Some(frontend) = self.frontend.filter(|_| self.kind.has_web()) {
            web.push_str(" --frontend ");
            web.push_str(&value_name(frontend));
            web.push(' ');
            web.push_str(&flag(self.tailwind, "tailwind"));
            if self.frontend == Some(Frontend::Mold) {
                web.push(' ');
                web.push_str(&flag(self.alpine, "alpine"));
            }
            web.push_str(" --smelt ");
            web.push_str(&self.blocks.to_string());
        }
        let npm = if self.is_js() {
            format!(" {}", flag(self.npm, "npm"))
        } else {
            String::new()
        };
        format!(
            "smeltery new {} --kind {kind} --db {db}{web} --bellows {}{npm} {} {} {}",
            self.name,
            self.bellows,
            flag(self.migrate, "migrate"),
            flag(self.seed, "seed"),
            flag(self.git, "git"),
        )
    }
}

/// Checks an app name: lowercase letters, digits, `-`, `_`, starting with a letter, not a reserved name.
pub(crate) fn validate_name(name: &str) -> anyhow::Result<()> {
    const RESERVED: &[&str] = &[
        "smeltery",
        "test",
        "std",
        "core",
        "alloc",
        "proc_macro",
        "abstract",
        "as",
        "async",
        "await",
        "become",
        "box",
        "break",
        "const",
        "continue",
        "crate",
        "do",
        "dyn",
        "else",
        "enum",
        "extern",
        "false",
        "final",
        "fn",
        "for",
        "gen",
        "if",
        "impl",
        "in",
        "let",
        "loop",
        "macro",
        "match",
        "mod",
        "move",
        "mut",
        "override",
        "priv",
        "pub",
        "ref",
        "return",
        "self",
        "static",
        "struct",
        "super",
        "trait",
        "true",
        "try",
        "type",
        "typeof",
        "unsafe",
        "unsized",
        "use",
        "virtual",
        "where",
        "while",
        "yield",
    ];
    let starts_with_letter = name.chars().next().is_some_and(|c| c.is_ascii_lowercase());
    let allowed = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !starts_with_letter || !allowed || name.len() > 64 {
        bail!(
            "invalid app name `{name}`: use lowercase letters, digits, `-` and `_`, start with a letter, at most 64 \
             characters"
        );
    }
    if RESERVED.contains(&lib_name(name).as_str()) {
        bail!("invalid app name `{name}`: it is a reserved Rust or crate name");
    }
    Ok(())
}

/// The Rust library name of an app (`my-app` → `my_app`).
pub(crate) fn lib_name(name: &str) -> String {
    name.replace('-', "_")
}

/// The display name of an app (`my-app` → `My App`).
pub(crate) fn title(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars
                .next()
                .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn database_url(db: Db, lib_name: &str) -> String {
    match db {
        Db::Sqlite => "sqlite://database/database.sqlite?mode=rwc".to_owned(),
        Db::Postgres => format!("postgres://postgres:postgres@127.0.0.1:5432/{lib_name}"),
        Db::Mysql => format!("mysql://root:root@127.0.0.1:3306/{lib_name}"),
    }
}

/// Removes `.` and folds `..` components without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// The path of `to` seen from the directory `from` (both absolute), e.g. `../../crates/smeltery`.
///
/// `None` when the two share no root (different drives on Windows). Components are joined with `/`, which
/// Cargo reads on every platform.
pub(crate) fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from = normalize(from);
    let to = normalize(to);
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    if from.first() != to.first() {
        return None;
    }
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_owned(); from.len() - common];
    parts.extend(
        to.iter()
            .skip(common)
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    if parts.is_empty() {
        return Some(PathBuf::from("."));
    }
    Some(PathBuf::from(parts.join("/")))
}

/// The `smeltery = …` dependency line of the generated `Cargo.toml`.
fn smeltery_dep(db: Db, local: Option<&Path>) -> String {
    let feature = db
        .to_possible_value()
        .map(|v| v.get_name().to_owned())
        .unwrap_or_default();
    let source = match local {
        Some(path) => {
            let path = path
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            format!("path = \"{path}\"")
        }
        None => format!("version = \"{}\"", env!("CARGO_PKG_VERSION")),
    };
    format!("smeltery = {{ {source}, default-features = false, features = [\"{feature}\"] }}")
}

/// Writes the app for `opts` into `target` (which must not exist or be empty) and returns the written paths.
///
/// `smeltery_crate` is the local `crates/smeltery` directory to depend on by path, if any.
pub(crate) fn generate(
    opts: &NewOptions,
    target: &Path,
    smeltery_crate: Option<&Path>,
    app_key: &str,
    now: u64,
) -> anyhow::Result<Vec<String>> {
    validate_name(&opts.name)?;
    if target.exists() {
        let mut entries = std::fs::read_dir(target).with_context(|| {
            format!(
                "{} exists and is not a readable directory",
                target.display()
            )
        })?;
        if entries.next().is_some() {
            bail!("{} already exists and is not empty", target.display());
        }
    }
    let lib = lib_name(&opts.name);
    let frontend = opts.frontend.filter(|_| opts.kind.has_web());
    let ctx = templates::Context {
        tailwind: opts.kind.has_web() && opts.tailwind,
        name: opts.name.clone(),
        lib_name: lib.clone(),
        title: title(&opts.name),
        web: opts.kind.has_web(),
        agents: opts.has_agents(),
        database_url: database_url(opts.db, &lib),
        smeltery_dep: smeltery_dep(opts.db, smeltery_crate),
        app_key: app_key.to_owned(),
        auth: opts.has_auth(),
        temper: opts.has_temper(),
        alpine: opts.kind.has_web() && opts.alpine,
        alpine_version: templates::ALPINE_VERSION.to_owned(),
        bellows_mcp: opts.bellows.mcp,
        bellows_skills: opts.bellows.skills,
        bellows_guidelines: opts.bellows.guidelines,
        users_migration: format!("m{}_create_users_table", stamp(now)),
        users_migration_name: format!("{}_create_users_table", stamp(now)),
        resets_migration: format!("m{}_create_password_reset_tokens_table", stamp(now + 1)),
        resets_migration_name: format!("{}_create_password_reset_tokens_table", stamp(now + 1)),
        sessions_migration: format!("m{}_create_sessions_table", stamp(now + 2)),
        sessions_migration_name: format!("{}_create_sessions_table", stamp(now + 2)),
        watchfire_migration: format!("m{}_create_watchfire_tables", stamp(now + 3)),
        watchfire_migration_name: format!("{}_create_watchfire_tables", stamp(now + 3)),
        cache_migration: format!("m{}_create_cache_tables", stamp(now + 4)),
        cache_migration_name: format!("{}_create_cache_tables", stamp(now + 4)),
        pubsub_migration: format!("m{}_create_pubsub_messages_table", stamp(now + 5)),
        pubsub_migration_name: format!("{}_create_pubsub_messages_table", stamp(now + 5)),
        two_factor_migration: format!("m{}_add_two_factor_columns_to_users_table", stamp(now + 6)),
        two_factor_migration_name: format!(
            "{}_add_two_factor_columns_to_users_table",
            stamp(now + 6)
        ),
        hallmark: opts.has_hallmark(),
        anvil: opts.has_anvil(),
        search: opts.has_search(),
        presence: opts.has_anvil() && opts.has_auth() && opts.is_js(),
        presence_migration: format!("m{}_create_presence_tables", stamp(now + 8)),
        presence_migration_name: format!("{}_create_presence_tables", stamp(now + 8)),
        hallmark_migration: format!("m{}_create_personal_access_tokens_table", stamp(now + 7)),
        hallmark_migration_name: format!("{}_create_personal_access_tokens_table", stamp(now + 7)),
        ..templates::Context::default()
    }
    .with_frontend(frontend);
    let mut written = Vec::new();
    for template in TEMPLATES {
        let wanted = template.fits_kit(&ctx)
            && match template.when {
                When::Always => true,
                When::Web => ctx.web,
                When::Agents => ctx.agents,
                When::PubSub => ctx.agents || ctx.anvil,
                When::Hallmark => ctx.web && ctx.hallmark,
                When::Anvil => ctx.web && ctx.anvil,
                When::AnvilAuth => ctx.web && ctx.anvil && ctx.auth,
                When::Search => ctx.web && ctx.search,
                When::WebAuth => ctx.web && ctx.auth,
                When::WebNoAuth => ctx.web && !ctx.auth,
                When::Temper => ctx.web && ctx.temper,
                When::AuthOrHeadless => ctx.auth || !ctx.web,
                When::Alpine => ctx.web && ctx.alpine,
                When::Tailwind => ctx.web && ctx.tailwind,
                When::NoTailwind => ctx.web && !ctx.tailwind,
                When::BellowsMcp => opts.bellows.mcp,
                When::BellowsSkills => opts.bellows.skills,
                When::BellowsSkillsWeb => opts.bellows.skills && ctx.web,
                When::BellowsSkillsAuth => opts.bellows.skills && ctx.web && ctx.auth,
                When::BellowsSkillsAgents => opts.bellows.skills && ctx.agents,
                When::BellowsSkillsHallmark => opts.bellows.skills && ctx.web && ctx.hallmark,
                When::BellowsSkillsAnvil => opts.bellows.skills && ctx.web && ctx.anvil,
                When::BellowsSkillsSearch => opts.bellows.skills && ctx.web && ctx.search,
                When::BellowsGuidelines => opts.bellows.guidelines,
            };
        if !wanted {
            continue;
        }
        let rel = templates::out_path(template, &ctx)?;
        let out = target.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let text = templates::render(template, &ctx)?;
        // `.env` holds APP_KEY: owner-only on Unix, as `key:generate` writes it (D-205, D-353).
        let mode = if rel == ".env" {
            crate::files::Mode::Private
        } else {
            crate::files::Mode::Default
        };
        crate::files::create_new(&out, text.as_bytes(), mode)
            .with_context(|| format!("cannot write {}", out.display()))?;
        written.push(rel);
    }
    Ok(written)
}

/// Steps shown as sections (the starter kit, Tailwind, Alpine.js and building-block questions are skipped for
/// headless apps, Alpine.js for React / Vue, npm for Mold and when no usable Node.js is found).
const SECTIONS: [&str; 11] = [
    "Project",
    "Database",
    "Starter kit",
    "Tailwind",
    "Alpine.js",
    "Building blocks",
    "Bellows",
    "npm",
    "Migrations",
    "Seeders",
    "Git",
];

const MOVE_HINT: &str = "↑↓ move · enter confirm";
const TOGGLE_HINT: &str = "↑↓ move · space toggle · enter confirm";
const CONFIRM_HINT: &str = "y / n · enter confirm";

const ALPINE_HELP: &str =
    "Alpine.js, written to public/assets/js/ and loaded by the layout (no download)";
const TAILWIND_HELP: &str =
    "the standalone tailwindcss binary (about 110 MB, kept per user), for `serve` and `build`";
const TAILWIND_JS_HELP: &str =
    "the tailwindcss npm packages and the Vite plugin; without them the kit's prebuilt stylesheet";
const NPM_HELP: &str = "npm install --no-audit --no-fund in the new app (about 100 MB of packages)";

const BLOCKS_QUESTION: &str = "Which building blocks should we smelt into your stack?";
const DATABASES: [&str; 3] = ["SQLite", "PostgreSQL", "MySQL"];
const BELLOWS: [&str; 3] = [
    "MCP server (.mcp.json)",
    "Skills (.bellows/skills)",
    "Guidelines (.bellows/guidelines.md)",
];

/// Ctrl-C or Esc in a prompt ends `new` without creating anything.
fn prompt_error(err: inquire::InquireError) -> anyhow::Error {
    match err {
        inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted => {
            anyhow::anyhow!("cancelled; nothing was created")
        }
        other => other.into(),
    }
}

/// Asks every question. With a React / Vue kit it also looks for Node.js (`detect`) and returns what it found, so
/// `run` does not look twice.
fn ask(
    args: &NewArgs,
    ui: Ui,
    detect: impl FnOnce() -> NodeCheck,
) -> anyhow::Result<(NewOptions, Option<NodeCheck>)> {
    use inquire::{Confirm, MultiSelect, Select};
    let cfg = ui.render_config();
    if ui.styled() {
        print!("{}", ui.banner(env!("CARGO_PKG_VERSION")));
    }
    let section = |step: usize, hint: &str| {
        if ui.styled() {
            let title = SECTIONS.get(step - 1).copied().unwrap_or_default();
            print!("{}", ui.section(step, 0, title, hint));
        }
    };
    let index = |items: &[&str], picked: &str| items.iter().position(|i| *i == picked).unwrap_or(0);

    section(1, MOVE_HINT);
    let labels: Vec<&str> = Kind::ALL.iter().map(|k| k.label()).collect();
    let picked = Select::new("What are you building?", labels)
        .without_help_message()
        .with_render_config(cfg)
        .prompt()
        .map_err(prompt_error)?;
    let kind = Kind::ALL
        .into_iter()
        .find(|k| k.label() == picked)
        .unwrap_or(Kind::Web);
    section(2, MOVE_HINT);
    let picked = Select::new("Database", DATABASES.to_vec())
        .without_help_message()
        .with_render_config(cfg)
        .prompt()
        .map_err(prompt_error)?;
    let db = match index(&DATABASES, picked) {
        1 => Db::Postgres,
        2 => Db::Mysql,
        _ => Db::Sqlite,
    };
    let frontend = if kind.has_web() {
        section(3, MOVE_HINT);
        let labels: Vec<&str> = Frontend::ALL.iter().map(|f| f.label()).collect();
        let picked = Select::new("Starter kit", labels)
            .without_help_message()
            .with_render_config(cfg)
            .prompt()
            .map_err(prompt_error)?;
        Some(
            Frontend::ALL
                .into_iter()
                .find(|f| f.label() == picked)
                .unwrap_or(Frontend::Mold),
        )
    } else {
        None
    };
    let js = frontend.is_some_and(Frontend::is_js);
    let tailwind = if kind.has_web() {
        section(4, CONFIRM_HINT);
        let (question, help) = if js {
            ("Tailwind CSS?", TAILWIND_JS_HELP)
        } else {
            ("Install Tailwind?", TAILWIND_HELP)
        };
        Confirm::new(question)
            .with_default(true)
            .with_help_message(help)
            .with_render_config(cfg)
            .prompt()
            .map_err(prompt_error)?
    } else {
        false
    };
    let alpine = if frontend == Some(Frontend::Mold) {
        section(5, CONFIRM_HINT);
        Confirm::new("Alpine.js?")
            .with_default(false)
            .with_help_message(ALPINE_HELP)
            .with_render_config(cfg)
            .prompt()
            .map_err(prompt_error)?
    } else {
        false
    };
    let blocks = if kind.has_web() {
        section(6, TOGGLE_HINT);
        let preselected = Blocks::preselected();
        let formatter: inquire::formatter::MultiOptionFormatter<'_, &str> = &|picked| {
            let labels: Vec<&str> = picked.iter().map(|p| *p.value).collect();
            Blocks::answer_for(&labels)
        };
        let picked = MultiSelect::new(BLOCKS_QUESTION, Blocks::labels())
            .with_default(&preselected)
            .with_formatter(formatter)
            .without_help_message()
            .with_render_config(cfg)
            .prompt()
            .map_err(prompt_error)?;
        let (blocks, added) = Blocks::from_labels(&picked).with_required();
        print_block_notes(ui, &added, &blocks.hints());
        blocks
    } else {
        Blocks::default()
    };
    section(7, TOGGLE_HINT);
    let picked = MultiSelect::new("Bellows AI-agent support", BELLOWS.to_vec())
        .without_help_message()
        .with_render_config(cfg)
        .prompt()
        .map_err(prompt_error)?;
    let has = |i: usize| BELLOWS.get(i).is_some_and(|b| picked.contains(b));
    let bellows = Bellows {
        mcp: has(0),
        skills: has(1),
        guidelines: has(2),
    };
    let confirm = |step: usize, question: &str| -> anyhow::Result<bool> {
        section(step, CONFIRM_HINT);
        Confirm::new(question)
            .with_default(true)
            .with_render_config(cfg)
            .prompt()
            .map_err(prompt_error)
    };
    // The npm question only when a usable Node.js is there; otherwise the step is skipped with a note in the summary.
    let node = js.then(detect);
    let npm = match &node {
        Some(NodeCheck::Ready { .. }) => {
            section(8, CONFIRM_HINT);
            Confirm::new("Install the npm packages now?")
                .with_default(true)
                .with_help_message(NPM_HELP)
                .with_render_config(cfg)
                .prompt()
                .map_err(prompt_error)?
        }
        Some(_) => true,
        None => false,
    };
    let migrate = confirm(9, "Run migrations?")?;
    let seed = confirm(10, "Run seeders?")?;
    let git = confirm(11, "Initialize a git repository?")?;
    if ui.styled() {
        println!();
    }
    let opts = NewOptions {
        name: args.name.clone(),
        kind,
        db,
        blocks,
        frontend,
        bellows,
        tailwind,
        alpine,
        npm,
        migrate,
        seed,
        git,
    };
    Ok((opts, node))
}

/// The notes for blocks chosen with `--smelt`: the blocks added because a chosen one needs them, and the hints for
/// blocks that pair well with a chosen one (web apps only).
fn flag_notes(args: &NewArgs, opts: &NewOptions) -> (Vec<String>, Vec<String>) {
    if !opts.kind.has_web() {
        return (Vec::new(), Vec::new());
    }
    let asked = args
        .smelt
        .as_deref()
        .map(Blocks::from_args)
        .unwrap_or_default();
    (asked.with_required().1, opts.blocks.hints())
}

/// Prints the notes under the building-blocks answer.
fn print_block_notes(ui: Ui, added: &[String], hints: &[String]) {
    for line in added.iter().chain(hints) {
        println!("{}", ui.note_line(line));
    }
}

/// Preview mode (styling forced, no prompts): the banner and every question shown as answered, with the block
/// notes under the building-blocks answer as the interactive flow prints them.
fn show_answers(opts: &NewOptions, ui: Ui, notes: &[String]) {
    print!("{}", ui.banner(env!("CARGO_PKG_VERSION")));
    let db = match opts.db {
        Db::Sqlite => DATABASES[0],
        Db::Postgres => DATABASES[1],
        Db::Mysql => DATABASES[2],
    };
    let yes_no = |b: bool| if b { "Yes" } else { "No" };
    let bellows = opts.bellows.to_string();
    let mut steps: Vec<(usize, &str, String)> = vec![
        (1, MOVE_HINT, opts.kind.label().to_owned()),
        (2, MOVE_HINT, db.to_owned()),
    ];
    if let Some(frontend) = opts.frontend {
        steps.push((3, MOVE_HINT, frontend.label().to_owned()));
        steps.push((4, CONFIRM_HINT, yes_no(opts.tailwind).to_owned()));
        if frontend == Frontend::Mold {
            steps.push((5, CONFIRM_HINT, yes_no(opts.alpine).to_owned()));
        }
        steps.push((6, TOGGLE_HINT, opts.blocks.answer()));
    }
    steps.push((7, TOGGLE_HINT, bellows));
    if opts.is_js() {
        steps.push((8, CONFIRM_HINT, yes_no(opts.npm).to_owned()));
    }
    steps.push((9, CONFIRM_HINT, yes_no(opts.migrate).to_owned()));
    steps.push((10, CONFIRM_HINT, yes_no(opts.seed).to_owned()));
    steps.push((11, CONFIRM_HINT, yes_no(opts.git).to_owned()));
    for (step, hint, answer) in steps {
        let title = SECTIONS.get(step - 1).copied().unwrap_or_default();
        print!(
            "{}{}",
            ui.section(step, 0, title, hint),
            ui.answered(&answer)
        );
        if step == 6 {
            print_block_notes(ui, notes, &[]);
        }
    }
    println!();
}

/// Refuses flag combinations that cannot be honoured: building blocks for a headless app (it always runs Watchfire
/// and has no web pages, so a block such as `hallmark` would be left out without a word).
fn check_flags(args: &NewArgs) -> anyhow::Result<()> {
    let blocks = args.smelt.as_deref().map(Blocks::from_args);
    if args.kind == Some(Kind::Headless) && blocks.is_some_and(|b| b != Blocks::NONE) {
        bail!(
            "--smelt: headless apps take no building blocks (they always run Watchfire and have no web pages); \
             leave out --smelt, or use --kind web"
        );
    }
    Ok(())
}

/// Runs `smeltery new` from `cwd`.
pub(crate) fn run(args: NewArgs, cwd: &Path, ui: Ui) -> anyhow::Result<()> {
    validate_name(&args.name)?;
    check_flags(&args)?;
    let parent = match &args.path {
        Some(p) => cwd.join(p),
        None => cwd.to_path_buf(),
    };
    let target = parent.join(&args.name);
    let smeltery_crate = match &args.smeltery_path {
        Some(root) => {
            let krate = std::path::absolute(cwd.join(root).join("crates").join("smeltery"))?;
            if !krate.join("Cargo.toml").is_file() {
                bail!("--smeltery-path: {} has no Cargo.toml", krate.display());
            }
            // A relative path stays relative (seen from the app's directory), so an app kept inside the
            // checkout (the demos under `examples/`) builds wherever the checkout is cloned.
            if root.is_relative() {
                let app_dir = std::path::absolute(&target)?;
                Some(relative_path(&app_dir, &krate).unwrap_or(krate))
            } else {
                Some(krate)
            }
        }
        None => None,
    };
    let interactive = !args.any_question_flag()
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal();
    let (node_tool, npm_tool) = (frontend::node_from_env(), frontend::npm_from_env());
    let (opts, node) = if interactive {
        ask(&args, ui, || frontend::detect(&node_tool, &npm_tool))?
    } else {
        let opts = NewOptions::from_flags(&args);
        let (added, hints) = flag_notes(&args, &opts);
        if ui.forced() {
            let notes: Vec<String> = added.into_iter().chain(hints).collect();
            show_answers(&opts, ui, &notes);
        } else {
            print_block_notes(ui, &added, &hints);
        }
        (opts, None)
    };

    let key = crate::key::generate()?;
    let written = generate(
        &opts,
        &target,
        smeltery_crate.as_deref(),
        &key,
        crate::make::Ctx::now_secs(),
    )?;
    if ui.styled() {
        for line in created_lines(&written) {
            println!("{}", ui.done_line(&line));
        }
    }

    // The React / Vue kits get Tailwind from npm (`@tailwindcss/vite`); only Mold apps use the standalone binary.
    let tailwind = match (opts.tailwind, opts.is_js()) {
        (false, _) => "no",
        (true, true) => "yes",
        (true, false) => install_tailwind(ui),
    };
    // Before `git init`, so the lock file npm writes is in the first commit.
    let npm = if !opts.is_js() {
        String::new()
    } else if opts.npm {
        let node = node.unwrap_or_else(|| frontend::detect(&node_tool, &npm_tool));
        npm_step(&target, &node, &npm_tool, ui)
    } else {
        "no".to_owned()
    };
    let git = if opts.git {
        git_init(&target, ui)
    } else {
        "no"
    };
    let migrated = if opts.migrate {
        app_command(&target, "migrate", ui)
    } else {
        "no"
    };
    let seeded = match (opts.seed, migrated) {
        (false, _) => "no",
        (true, "done") => app_command(&target, "db:seed", ui),
        (true, _) => {
            if ui.styled() {
                println!(
                    "  {}",
                    ui.badged(
                        Badge::Info,
                        "db:seed skipped: the database is not migrated (run `smeltery migrate` first)"
                    )
                );
            } else {
                println!(
                    "db:seed skipped: the database is not migrated (run `smeltery migrate` first)"
                );
            }
            "skipped"
        }
    };
    let shown = target
        .strip_prefix(cwd)
        .unwrap_or(&target)
        .display()
        .to_string();
    let steps = Steps {
        tailwind,
        npm: &npm,
        migrated,
        seeded,
        git,
    };
    if ui.styled() {
        println!();
        print!("{}", styled_summary(&opts, ui, &shown, &steps));
        println!();
        println!(
            "  {}",
            ui.badged(Badge::Done, &format!("{} is ready", opts.name))
        );
    } else {
        print!("{}", plain_summary(&opts, &target, &shown, &steps));
    }
    Ok(())
}

/// What the steps after writing the files did, for the summary.
#[derive(Debug, Clone, Copy)]
struct Steps<'a> {
    tailwind: &'a str,
    npm: &'a str,
    migrated: &'a str,
    seeded: &'a str,
    git: &'a str,
}

/// The npm step of `smeltery new`: `npm install` when Node.js is usable, otherwise skipped with a warning (the app
/// is complete without `node_modules`).
fn npm_step(dir: &Path, node: &NodeCheck, npm: &crate::cmd::Tool, ui: Ui) -> String {
    if matches!(node, NodeCheck::Ready { .. }) {
        return frontend::npm_install(dir, npm, ui).to_owned();
    }
    let found = match node {
        NodeCheck::TooOld { node } => format!("Node.js {node} is too old"),
        _ => "Node.js or npm was not found".to_owned(),
    };
    let message = format!(
        "npm install skipped: {found} ({} needed). Install it, then run `npm install` in the app",
        frontend::NODE_REQUIREMENT
    );
    if ui.styled() {
        eprintln!("{}", ui.badged(Badge::Warn, &message));
    } else {
        eprintln!("warning: {message}");
    }
    node.skipped()
}

/// `yes` / `no` for a summary row.
fn on_off(on: bool) -> &'static str {
    if on { "yes" } else { "no" }
}

pub(crate) fn value_name(v: impl ValueEnum) -> String {
    v.to_possible_value()
        .map(|v| v.get_name().to_owned())
        .unwrap_or_default()
}

/// The summary as `smeltery new` printed it before styling existed; plain mode prints exactly this.
fn plain_summary(opts: &NewOptions, target: &Path, shown: &str, steps: &Steps<'_>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "Created {} in {}", opts.name, target.display());
    for (key, value) in summary_rows(opts, steps) {
        let _ = writeln!(out, "  {key:<10}{value}");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Same app without questions:");
    let _ = writeln!(out, "  {}", opts.command_line());
    let _ = writeln!(out);
    let _ = writeln!(out, "Next steps:");
    for step in next_steps(shown, steps) {
        let _ = writeln!(out, "  {step}");
    }
    out
}

/// The summary rows, in the order of the questions.
fn summary_rows(opts: &NewOptions, steps: &Steps<'_>) -> Vec<(&'static str, String)> {
    let mut rows: Vec<(&str, String)> = vec![
        ("kind", value_name(opts.kind)),
        ("database", value_name(opts.db)),
    ];
    if let Some(frontend) = opts.frontend {
        rows.push(("frontend", frontend.label().to_owned()));
        rows.push(("tailwind", steps.tailwind.to_owned()));
        if frontend == Frontend::Mold {
            rows.push(("alpine", on_off(opts.alpine).to_owned()));
        }
        rows.push(("smelt", opts.blocks.to_string()));
    }
    rows.push(("bellows", opts.bellows.to_string()));
    if opts.is_js() {
        rows.push(("npm", steps.npm.to_owned()));
    }
    rows.push(("migrate", steps.migrated.to_owned()));
    rows.push(("seed", steps.seeded.to_owned()));
    rows.push(("git", steps.git.to_owned()));
    rows
}

/// The summary box shown when styling is on.
fn styled_summary(opts: &NewOptions, ui: Ui, shown: &str, steps: &Steps<'_>) -> String {
    ui.summary_box(
        &format!("Created {}", opts.name),
        &summary_rows(opts, steps),
        &opts.command_line(),
        &next_steps(shown, steps),
    )
}

/// The next steps of the summary: a failed Tailwind download adds the command that retries it, a React / Vue app
/// without its npm packages starts with `npm install`.
fn next_steps(shown: &str, steps: &Steps<'_>) -> Vec<String> {
    // `npm` is empty for apps without a JavaScript kit.
    let mut next = if !steps.npm.is_empty() && steps.npm != "installed" {
        vec![format!("cd {shown} && npm install && smeltery serve")]
    } else {
        vec![format!("cd {shown} && smeltery serve")]
    };
    if steps.tailwind == "failed" {
        next.push("smeltery tailwind:install   (retries the Tailwind CSS download)".to_owned());
    }
    next
}

/// Downloads the pinned Tailwind binary for `smeltery new`. A failure is a warning: the app keeps its prebuilt CSS.
fn install_tailwind(ui: Ui) -> &'static str {
    use crate::tailwind::{Installed, Installer, VERSION, install_with_progress};
    match Installer::for_user().and_then(|installer| install_with_progress(&installer, ui)) {
        Ok(installed) => {
            let state = match installed {
                Installed::Downloaded(_) => "installed",
                Installed::Reused(_) => "already installed",
            };
            let line = format!(
                "Tailwind CSS v{VERSION} {state} at {}",
                installed.path().display()
            );
            if ui.styled() {
                println!("{}", ui.done_line(&line));
            } else {
                println!("{line}");
            }
            state
        }
        Err(err) => {
            let message = format!(
                "Tailwind CSS was not installed: {err:#}. The app keeps its prebuilt CSS; retry with \
                 `smeltery tailwind:install`"
            );
            if ui.styled() {
                println!("{}", ui.fail_line("tailwind", "download failed"));
                eprintln!("{}", ui.badged(Badge::Warn, &message));
            } else {
                eprintln!("warning: {message}");
            }
            "failed"
        }
    }
}

/// Progress lines for the generated files: one per top-level folder (with its file count) or root file.
fn created_lines(written: &[String]) -> Vec<String> {
    let mut groups: Vec<(String, usize, bool)> = Vec::new();
    for path in written {
        let path = path.replace('\\', "/");
        let mut parts = path.split('/');
        let first = parts.next().unwrap_or_default().to_owned();
        let is_dir = parts.next().is_some();
        match groups
            .iter_mut()
            .find(|(name, _, dir)| *name == first && *dir == is_dir)
        {
            Some(group) => group.1 += 1,
            None => groups.push((first, 1, is_dir)),
        }
    }
    groups
        .into_iter()
        .map(|(name, count, dir)| {
            if dir {
                let files = if count == 1 { "file" } else { "files" };
                format!("{name}/ · {count} {files}")
            } else {
                name
            }
        })
        .collect()
}

/// Runs `cargo run --quiet -- <command>` in the new app. A failure is a warning with the command to run later.
fn app_command(dir: &Path, command: &str, ui: Ui) -> &'static str {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    if !ui.styled() {
        println!("Running `{command}` (the first build of the app takes a while)...");
        let status = std::process::Command::new(cargo)
            .args(["run", "--quiet", "--", command])
            .current_dir(dir)
            .status();
        return match status {
            Ok(s) if s.success() => "done",
            _ => {
                eprintln!(
                    "warning: `{command}` failed; run it later with: cd {} && smeltery {command}",
                    dir.display()
                );
                "failed"
            }
        };
    }
    // Styled: the build and command output stay hidden behind the spinner and appear only on failure.
    let spinner = ui.spinner(&format!(
        "Running {command} (the first build of the app takes a while)"
    ));
    let output = std::process::Command::new(cargo)
        .args(["run", "--quiet", "--", command])
        .current_dir(dir)
        .output();
    spinner.finish();
    match output {
        Ok(out) if out.status.success() => {
            println!("{}", ui.done_line(command));
            "done"
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let reason = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("it exited with an error")
                .trim()
                .to_owned();
            println!("{}", ui.fail_line(command, &reason));
            eprintln!(
                "{}",
                ui.badged(
                    Badge::Warn,
                    &format!(
                        "run it later with: cd {} && smeltery {command}",
                        dir.display()
                    )
                )
            );
            "failed"
        }
        Err(err) => {
            println!("{}", ui.fail_line(command, &err.to_string()));
            "failed"
        }
    }
}

/// Runs `git init`; a missing or failing git is a warning, not an error.
fn git_init(dir: &Path, ui: Ui) -> &'static str {
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir)
        .status();
    match status {
        Ok(status) if status.success() => {
            if ui.styled() {
                println!("{}", ui.done_line("git repository"));
            }
            "initialized"
        }
        Ok(_) => {
            if ui.styled() {
                println!("{}", ui.fail_line("git repository", "`git init` failed"));
            } else {
                eprintln!("warning: `git init` failed; the app was created without a repository");
            }
            "failed"
        }
        Err(_) => {
            if ui.styled() {
                eprintln!(
                    "{}",
                    ui.badged(
                        Badge::Warn,
                        "git not found; the app was created without a repository"
                    )
                );
            } else {
                eprintln!("warning: git not found; the app was created without a repository");
            }
            "skipped (git not found)"
        }
    }
}

/// Test shorthand for the three app shapes the generator writes: a web app with Watchfire, a web app without it,
/// and a headless app.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    WebWatchfire,
    Web,
    Headless,
}

#[cfg(test)]
impl Shape {
    pub(crate) fn kind(self) -> Kind {
        match self {
            Shape::Headless => Kind::Headless,
            Shape::WebWatchfire | Shape::Web => Kind::Web,
        }
    }

    pub(crate) fn has_web(self) -> bool {
        self != Shape::Headless
    }

    pub(crate) fn has_agents(self) -> bool {
        self != Shape::Web
    }

    /// The building blocks of this shape with the given authentication answer (no Hallmark, no Anvil).
    pub(crate) fn blocks(self, auth: bool) -> Blocks {
        Blocks {
            watchfire: self != Shape::Web,
            auth,
            hallmark: false,
            anvil: false,
            search: false,
        }
    }
}

#[cfg(test)]
mod tests;
