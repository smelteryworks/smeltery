//! The files `smeltery new` writes, embedded in the binary, and their rendering.

use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use serde::Serialize;

/// Which apps get a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum When {
    Always,
    /// Apps with HTTP routes (`--kind web`).
    Web,
    /// Apps with agents (headless apps, and web apps with the Watchfire block).
    Agents,
    /// Apps whose processes talk through PubSub: apps with agents, and web apps with Anvil (D-507, FEATURES.md).
    PubSub,
    /// Web apps with Hallmark's API tokens.
    Hallmark,
    /// Web apps with Anvil's WebSockets and broadcasting.
    Anvil,
    /// Web apps with Anvil and authentication (private and presence channel examples).
    AnvilAuth,
    /// Web apps with Prospect's search.
    Search,
    /// Web apps with the authentication scaffolding.
    WebAuth,
    /// Web apps without the authentication scaffolding.
    WebNoAuth,
    /// Web apps whose authentication is Temper (`app/providers/temper.rs`).
    Temper,
    /// Apps with the authentication scaffolding, and headless apps (which keep the users and password reset
    /// tables they always had).
    AuthOrHeadless,
    /// Web apps with Alpine.js.
    Alpine,
    /// Web apps with Tailwind (for the JavaScript kits: the npm packages instead of the prebuilt stylesheet).
    Tailwind,
    /// Web apps without Tailwind.
    NoTailwind,
    BellowsMcp,
    BellowsSkills,
    /// Skills that only apply to web apps.
    BellowsSkillsWeb,
    /// Skills that only apply to web apps with the authentication scaffolding.
    BellowsSkillsAuth,
    /// Skills that only apply to apps with agents.
    BellowsSkillsAgents,
    /// Skills that only apply to web apps with Hallmark.
    BellowsSkillsHallmark,
    /// Skills that only apply to web apps with Anvil.
    BellowsSkillsAnvil,
    /// Skills that only apply to web apps with Search.
    BellowsSkillsSearch,
    BellowsGuidelines,
}

/// Which starter kit gets a template (on top of [`When`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kit {
    /// Every app.
    Any,
    /// Apps without a JavaScript kit: the Mold frontend and headless apps (`templates/new/`).
    Mold,
    /// The React and the Vue kit (`templates/new-alloy/`: the Rust side, the root template, the CSS).
    Js,
    /// The React kit (`templates/new-react/`).
    React,
    /// The Vue kit (`templates/new-vue/`).
    Vue,
}

/// One output file of `smeltery new`.
#[derive(Debug)]
pub(crate) struct Template {
    /// Source name under its kit's template folder; a `.jinja` suffix means the source is rendered.
    pub(crate) src: &'static str,
    /// Output path relative to the app root.
    pub(crate) out: &'static str,
    pub(crate) source: &'static str,
    pub(crate) when: When,
    pub(crate) kit: Kit,
}

impl Template {
    pub(crate) fn is_rendered(&self) -> bool {
        self.src.ends_with(".jinja")
    }

    /// Whether the app's starter kit (in `ctx`) gets this template.
    pub(crate) fn fits_kit(&self, ctx: &Context) -> bool {
        match self.kit {
            Kit::Any => true,
            Kit::Mold => !ctx.alloy,
            Kit::Js => ctx.alloy,
            Kit::React => ctx.react,
            Kit::Vue => ctx.vue,
        }
    }
}

macro_rules! tpl {
    ($src:literal => $out:literal, $when:ident) => {
        tpl!(@ "new", Any, $src => $out, $when)
    };
    (mold $src:literal => $out:literal, $when:ident) => {
        tpl!(@ "new", Mold, $src => $out, $when)
    };
    (js $src:literal => $out:literal, $when:ident) => {
        tpl!(@ "new-alloy", Js, $src => $out, $when)
    };
    (react $src:literal => $out:literal, $when:ident) => {
        tpl!(@ "new-react", React, $src => $out, $when)
    };
    (vue $src:literal => $out:literal, $when:ident) => {
        tpl!(@ "new-vue", Vue, $src => $out, $when)
    };
    (hallmark $src:literal => $out:literal, $when:ident) => {
        tpl!(@ "hallmark", Any, $src => $out, $when)
    };
    (@ $dir:literal, $kit:ident, $src:literal => $out:literal, $when:ident) => {
        Template {
            src: $src,
            out: $out,
            source: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/templates/",
                $dir,
                "/",
                $src
            )),
            when: When::$when,
            kit: Kit::$kit,
        }
    };
    (keep $out:literal, $when:ident) => {
        Template {
            src: "",
            out: $out,
            source: "",
            when: When::$when,
            kit: Kit::Any,
        }
    };
}

/// The Alpine.js release embedded in the CLI (`templates/new/public/assets/js/alpine.min.js`). Upgrading Alpine is a
/// Smeltery release: replace the file (keeping its licence header), change this constant and the checksum in the
/// template test.
pub(crate) const ALPINE_VERSION: &str = "3.17.4";

/// Every file of a new app. Output paths are fixed; only contents depend on the options.
pub(crate) const TEMPLATES: &[Template] = &[
    tpl!("Cargo.toml.jinja" => "Cargo.toml", Always),
    tpl!("README.md.jinja" => "README.md", Always),
    tpl!("CLAUDE.md.jinja" => "CLAUDE.md", Always),
    tpl!("CLAUDE.md.jinja" => "AGENTS.md", Always),
    tpl!("gitignore.jinja" => ".gitignore", Always),
    tpl!("env.jinja" => ".env", Always),
    tpl!("env.jinja" => ".env.example", Always),
    tpl!("bootstrap/main.rs.jinja" => "bootstrap/main.rs", Always),
    tpl!("bootstrap/app.rs.jinja" => "bootstrap/app.rs", Always),
    tpl!("app/mod.rs.jinja" => "app/mod.rs", Always),
    tpl!("app/agents/mod.rs.jinja" => "app/agents/mod.rs", Agents),
    tpl!("app/commands/mod.rs" => "app/commands/mod.rs", Always),
    tpl!("app/controllers/mod.rs.jinja" => "app/controllers/mod.rs", Web),
    tpl!(mold "app/controllers/home.rs" => "app/controllers/home.rs", Web),
    tpl!(mold "app/controllers/dashboard.rs" => "app/controllers/dashboard.rs", WebAuth),
    tpl!(mold "app/controllers/settings.rs" => "app/controllers/settings.rs", Temper),
    tpl!(js "app/controllers/home.rs.jinja" => "app/controllers/home.rs", Web),
    tpl!(js "app/controllers/dashboard.rs.jinja" => "app/controllers/dashboard.rs", WebAuth),
    tpl!(js "app/controllers/settings.rs.jinja" => "app/controllers/settings.rs", Temper),
    tpl!("app/controllers/api/mod.rs" => "app/controllers/api/mod.rs", Hallmark),
    tpl!("app/controllers/api/tokens.rs" => "app/controllers/api/tokens.rs", Hallmark),
    tpl!("app/controllers/api/user.rs" => "app/controllers/api/user.rs", Hallmark),
    tpl!("app/events/mod.rs.jinja" => "app/events/mod.rs", Anvil),
    tpl!("app/events/announcement_posted.rs" => "app/events/announcement_posted.rs", Anvil),
    tpl!("app/events/user_notified.rs" => "app/events/user_notified.rs", AnvilAuth),
    tpl!(js "app/controllers/notifications.rs" => "app/controllers/notifications.rs", AnvilAuth),
    tpl!("app/actions/mod.rs" => "app/actions/mod.rs", Temper),
    tpl!("app/actions/temper/mod.rs" => "app/actions/temper/mod.rs", Temper),
    tpl!("app/actions/temper/create_new_user.rs" => "app/actions/temper/create_new_user.rs", Temper),
    tpl!("app/actions/temper/password_rules.rs" => "app/actions/temper/password_rules.rs", Temper),
    tpl!("app/actions/temper/reset_user_password.rs" => "app/actions/temper/reset_user_password.rs", Temper),
    tpl!("app/actions/temper/update_user_password.rs" => "app/actions/temper/update_user_password.rs", Temper),
    tpl!("app/actions/temper/update_user_profile_information.rs" => "app/actions/temper/update_user_profile_information.rs", Temper),
    tpl!("app/helpers/mod.rs" => "app/helpers/mod.rs", Always),
    tpl!("app/jobs/mod.rs" => "app/jobs/mod.rs", Always),
    tpl!("app/mail/mod.rs" => "app/mail/mod.rs", Always),
    tpl!("app/middleware/mod.rs" => "app/middleware/mod.rs", Always),
    tpl!("app/models/mod.rs" => "app/models/mod.rs", Always),
    tpl!("app/models/user.rs.jinja" => "app/models/user.rs", Always),
    tpl!(mold "app/providers/mod.rs.jinja" => "app/providers/mod.rs", Always),
    tpl!(mold "app/providers/temper.rs" => "app/providers/temper.rs", Temper),
    tpl!(js "app/providers/mod.rs.jinja" => "app/providers/mod.rs", Always),
    tpl!(js "app/providers/alloy.rs.jinja" => "app/providers/alloy.rs", Always),
    tpl!(js "app/providers/temper.rs.jinja" => "app/providers/temper.rs", Temper),
    tpl!("app/providers/search.rs" => "app/providers/search.rs", Search),
    tpl!("app/services/mod.rs" => "app/services/mod.rs", Always),
    tpl!(mold "app/sparks/mod.rs.jinja" => "app/sparks/mod.rs", Web),
    tpl!(mold "app/sparks/announcements.rs" => "app/sparks/announcements.rs", Anvil),
    tpl!(mold "app/sparks/notifications.rs" => "app/sparks/notifications.rs", AnvilAuth),
    tpl!(mold "app/sparks/counter.rs" => "app/sparks/counter.rs", Web),
    tpl!("config/mod.rs" => "config/mod.rs", Always),
    tpl!("config/app.rs.jinja" => "config/app.rs", Always),
    tpl!("config/database.rs.jinja" => "config/database.rs", Always),
    tpl!("routes/mod.rs.jinja" => "routes/mod.rs", Web),
    tpl!("routes/web.rs.jinja" => "routes/web.rs", Web),
    tpl!("routes/api.rs.jinja" => "routes/api.rs", Web),
    tpl!("routes/channels.rs.jinja" => "routes/channels.rs", Anvil),
    tpl!("database/mod.rs" => "database/mod.rs", Always),
    tpl!("database/migrations/mod.rs.jinja" => "database/migrations/mod.rs", Always),
    tpl!("database/migrations/create_users_table.rs.jinja" => "database/migrations/{{ users_migration }}.rs", Always),
    tpl!("database/migrations/create_password_reset_tokens_table.rs.jinja" => "database/migrations/{{ resets_migration }}.rs", AuthOrHeadless),
    tpl!("database/migrations/create_sessions_table.rs.jinja" => "database/migrations/{{ sessions_migration }}.rs", Always),
    tpl!("database/migrations/create_watchfire_tables.rs.jinja" => "database/migrations/{{ watchfire_migration }}.rs", Agents),
    tpl!("database/migrations/create_cache_tables.rs.jinja" => "database/migrations/{{ cache_migration }}.rs", Always),
    tpl!("database/migrations/create_pubsub_messages_table.rs.jinja" => "database/migrations/{{ pubsub_migration }}.rs", PubSub),
    tpl!("database/migrations/add_two_factor_columns_to_users_table.rs.jinja" => "database/migrations/{{ two_factor_migration }}.rs", Temper),
    // The same template as `smeltery hallmark:install` (D-467).
    tpl!(hallmark "create_personal_access_tokens_table.rs.jinja" => "database/migrations/{{ hallmark_migration }}.rs", Hallmark),
    // The presence channel of the React / Vue dashboard: its members' tables for the `database` PubSub driver.
    tpl!(js "database/migrations/create_presence_tables.rs.jinja" => "database/migrations/{{ presence_migration }}.rs", AnvilAuth),
    tpl!("database/seeders/mod.rs" => "database/seeders/mod.rs", Always),
    tpl!("database/seeders/database_seeder.rs.jinja" => "database/seeders/database_seeder.rs", AuthOrHeadless),
    tpl!("database/seeders/database_seeder_empty.rs" => "database/seeders/database_seeder.rs", WebNoAuth),
    tpl!("database/factories/mod.rs" => "database/factories/mod.rs", Always),
    tpl!("database/factories/user_factory.rs.jinja" => "database/factories/user_factory.rs", Always),
    tpl!(mold "resources/views/layouts/app.mold.html.jinja" => "resources/views/layouts/app.mold.html", Web),
    tpl!(mold "resources/views/home.mold.html.jinja" => "resources/views/home.mold.html", Web),
    tpl!(mold "resources/views/dashboard.mold.html.jinja" => "resources/views/dashboard.mold.html", WebAuth),
    tpl!(mold "resources/views/sparks/announcements.mold.html" => "resources/views/sparks/announcements.mold.html", Anvil),
    tpl!(mold "resources/views/sparks/notifications.mold.html" => "resources/views/sparks/notifications.mold.html", AnvilAuth),
    tpl!(mold "resources/views/sparks/counter.mold.html.jinja" => "resources/views/sparks/counter.mold.html", Web),
    tpl!(mold "resources/views/auth/login.mold.html" => "resources/views/auth/login.mold.html", WebAuth),
    tpl!(mold "resources/views/auth/register.mold.html" => "resources/views/auth/register.mold.html", WebAuth),
    tpl!(mold "resources/views/auth/forgot-password.mold.html" => "resources/views/auth/forgot-password.mold.html", WebAuth),
    tpl!(mold "resources/views/auth/reset-password.mold.html" => "resources/views/auth/reset-password.mold.html", WebAuth),
    tpl!(mold "resources/views/auth/verify-email.mold.html" => "resources/views/auth/verify-email.mold.html", WebAuth),
    tpl!(mold "resources/views/auth/confirm-password.mold.html" => "resources/views/auth/confirm-password.mold.html", Temper),
    tpl!(mold "resources/views/auth/two-factor-challenge.mold.html" => "resources/views/auth/two-factor-challenge.mold.html", Temper),
    tpl!(mold "resources/views/settings/nav.mold.html" => "resources/views/settings/nav.mold.html", Temper),
    tpl!(mold "resources/views/settings/profile.mold.html" => "resources/views/settings/profile.mold.html", Temper),
    tpl!(mold "resources/views/settings/password.mold.html" => "resources/views/settings/password.mold.html", Temper),
    tpl!(mold "resources/views/settings/two-factor.mold.html" => "resources/views/settings/two-factor.mold.html", Temper),
    tpl!(mold "resources/views/components/card.mold.html" => "resources/views/components/card.mold.html", Web),
    tpl!(mold "resources/css/app.css" => "resources/css/app.css", Web),
    // Compiled with the pinned Tailwind from every class of the view templates (D-206).
    tpl!(mold "public/assets/css/app.css" => "public/assets/css/app.css", Web),
    // The React and Vue kits (D-283, D-286): the Rust side and the root template are shared, the pages are not.
    tpl!(js "resources/views/app.mold.html.jinja" => "resources/views/app.mold.html", Web),
    tpl!(js "resources/css/app.css" => "resources/css/app.css", Tailwind),
    // Without Tailwind: what the pinned Tailwind built from both kits' pages (D-206, D-287).
    tpl!(js "resources/css/app.prebuilt.css" => "resources/css/app.css", NoTailwind),
    tpl!(react "package.json.jinja" => "package.json", Web),
    tpl!(react "tsconfig.json" => "tsconfig.json", Web),
    tpl!(react "vite.config.ts.jinja" => "vite.config.ts", Web),
    tpl!(react "resources/js/app.tsx.jinja" => "resources/js/app.tsx", Web),
    tpl!(react "resources/js/echo.ts" => "resources/js/echo.ts", Anvil),
    tpl!(react "resources/js/components/announcements.tsx" => "resources/js/components/announcements.tsx", Anvil),
    tpl!(react "resources/js/components/live-notifications.tsx" => "resources/js/components/live-notifications.tsx", AnvilAuth),
    tpl!(react "resources/js/types/global.d.ts.jinja" => "resources/js/types/global.d.ts", Web),
    tpl!(react "resources/js/layouts/app-layout.tsx.jinja" => "resources/js/layouts/app-layout.tsx", Web),
    tpl!(react "resources/js/layouts/auth-layout.tsx" => "resources/js/layouts/auth-layout.tsx", WebAuth),
    tpl!(react "resources/js/components/app-logo.tsx" => "resources/js/components/app-logo.tsx", Web),
    tpl!(react "resources/js/components/card.tsx" => "resources/js/components/card.tsx", Web),
    tpl!(react "resources/js/components/ingot.tsx" => "resources/js/components/ingot.tsx", Web),
    tpl!(react "resources/js/components/input-error.tsx" => "resources/js/components/input-error.tsx", WebAuth),
    tpl!(react "resources/js/components/text-input.tsx" => "resources/js/components/text-input.tsx", WebAuth),
    tpl!(react "resources/js/pages/welcome.tsx.jinja" => "resources/js/pages/welcome.tsx", Web),
    tpl!(react "resources/js/pages/dashboard.tsx.jinja" => "resources/js/pages/dashboard.tsx", WebAuth),
    tpl!(react "resources/js/pages/auth/login.tsx" => "resources/js/pages/auth/login.tsx", WebAuth),
    tpl!(react "resources/js/pages/auth/register.tsx" => "resources/js/pages/auth/register.tsx", WebAuth),
    tpl!(react "resources/js/pages/auth/forgot-password.tsx" => "resources/js/pages/auth/forgot-password.tsx", WebAuth),
    tpl!(react "resources/js/pages/auth/reset-password.tsx" => "resources/js/pages/auth/reset-password.tsx", WebAuth),
    tpl!(react "resources/js/pages/auth/verify-email.tsx" => "resources/js/pages/auth/verify-email.tsx", WebAuth),
    tpl!(react "resources/js/pages/auth/confirm-password.tsx" => "resources/js/pages/auth/confirm-password.tsx", Temper),
    tpl!(react "resources/js/pages/auth/two-factor-challenge.tsx" => "resources/js/pages/auth/two-factor-challenge.tsx", Temper),
    tpl!(react "resources/js/layouts/settings-layout.tsx" => "resources/js/layouts/settings-layout.tsx", Temper),
    tpl!(react "resources/js/pages/settings/profile.tsx" => "resources/js/pages/settings/profile.tsx", Temper),
    tpl!(react "resources/js/pages/settings/password.tsx" => "resources/js/pages/settings/password.tsx", Temper),
    tpl!(react "resources/js/pages/settings/two-factor.tsx" => "resources/js/pages/settings/two-factor.tsx", Temper),
    tpl!(vue "package.json.jinja" => "package.json", Web),
    tpl!(vue "tsconfig.json" => "tsconfig.json", Web),
    tpl!(vue "vite.config.ts.jinja" => "vite.config.ts", Web),
    tpl!(vue "resources/js/app.ts.jinja" => "resources/js/app.ts", Web),
    tpl!(vue "resources/js/echo.ts" => "resources/js/echo.ts", Anvil),
    tpl!(vue "resources/js/components/AnnouncementsCard.vue" => "resources/js/components/AnnouncementsCard.vue", Anvil),
    tpl!(vue "resources/js/components/LiveNotifications.vue" => "resources/js/components/LiveNotifications.vue", AnvilAuth),
    tpl!(vue "resources/js/types/global.d.ts.jinja" => "resources/js/types/global.d.ts", Web),
    tpl!(vue "resources/js/layouts/AppLayout.vue.jinja" => "resources/js/layouts/AppLayout.vue", Web),
    tpl!(vue "resources/js/layouts/AuthLayout.vue" => "resources/js/layouts/AuthLayout.vue", WebAuth),
    tpl!(vue "resources/js/components/AppLogo.vue" => "resources/js/components/AppLogo.vue", Web),
    tpl!(vue "resources/js/components/AppCard.vue" => "resources/js/components/AppCard.vue", Web),
    tpl!(vue "resources/js/components/ForgeIngot.vue" => "resources/js/components/ForgeIngot.vue", Web),
    tpl!(vue "resources/js/components/InputError.vue" => "resources/js/components/InputError.vue", WebAuth),
    tpl!(vue "resources/js/components/TextInput.vue" => "resources/js/components/TextInput.vue", WebAuth),
    tpl!(vue "resources/js/pages/Welcome.vue.jinja" => "resources/js/pages/Welcome.vue", Web),
    tpl!(vue "resources/js/pages/Dashboard.vue.jinja" => "resources/js/pages/Dashboard.vue", WebAuth),
    tpl!(vue "resources/js/pages/auth/Login.vue" => "resources/js/pages/auth/Login.vue", WebAuth),
    tpl!(vue "resources/js/pages/auth/Register.vue" => "resources/js/pages/auth/Register.vue", WebAuth),
    tpl!(vue "resources/js/pages/auth/ForgotPassword.vue" => "resources/js/pages/auth/ForgotPassword.vue", WebAuth),
    tpl!(vue "resources/js/pages/auth/ResetPassword.vue" => "resources/js/pages/auth/ResetPassword.vue", WebAuth),
    tpl!(vue "resources/js/pages/auth/VerifyEmail.vue" => "resources/js/pages/auth/VerifyEmail.vue", WebAuth),
    tpl!(vue "resources/js/pages/auth/ConfirmPassword.vue" => "resources/js/pages/auth/ConfirmPassword.vue", Temper),
    tpl!(vue "resources/js/pages/auth/TwoFactorChallenge.vue" => "resources/js/pages/auth/TwoFactorChallenge.vue", Temper),
    tpl!(vue "resources/js/layouts/SettingsLayout.vue" => "resources/js/layouts/SettingsLayout.vue", Temper),
    tpl!(vue "resources/js/pages/settings/Profile.vue" => "resources/js/pages/settings/Profile.vue", Temper),
    tpl!(vue "resources/js/pages/settings/Password.vue" => "resources/js/pages/settings/Password.vue", Temper),
    tpl!(vue "resources/js/pages/settings/TwoFactor.vue" => "resources/js/pages/settings/TwoFactor.vue", Temper),
    tpl!(keep "public/assets/images/.gitkeep", Always),
    tpl!(keep "public/assets/css/.gitkeep", Always),
    tpl!(keep "public/assets/js/.gitkeep", Always),
    // The pinned Alpine.js release with its licence header ([`ALPINE_VERSION`], D-233).
    tpl!("public/assets/js/alpine.min.js" => "public/assets/js/alpine.min.js", Alpine),
    tpl!("robots.txt" => "public/robots.txt", Always),
    tpl!("storage.gitignore" => "storage/app/public/.gitignore", Always),
    tpl!("storage.gitignore" => "storage/app/private/.gitignore", Always),
    tpl!("storage.gitignore" => "storage/framework/.gitignore", Always),
    tpl!("storage.gitignore" => "storage/logs/.gitignore", Always),
    tpl!(mold "tests/http.rs.jinja" => "tests/http.rs", Always),
    tpl!(js "tests/http.rs.jinja" => "tests/http.rs", Always),
    tpl!("tests/api_tokens.rs.jinja" => "tests/api_tokens.rs", Hallmark),
    tpl!("tests/broadcasting.rs.jinja" => "tests/broadcasting.rs", Anvil),
    tpl!("bellows/guidelines.md.jinja" => ".bellows/guidelines.md", BellowsGuidelines),
    tpl!("bellows/skills/crud-resource.md.jinja" => ".bellows/skills/crud-resource.md", BellowsSkillsWeb),
    tpl!("bellows/skills/auth-route.md.jinja" => ".bellows/skills/auth-route.md", BellowsSkillsAuth),
    tpl!(mold "bellows/skills/spark.md.jinja" => ".bellows/skills/spark.md", BellowsSkillsWeb),
    tpl!(js "bellows/skills/alloy-page.md.jinja" => ".bellows/skills/alloy-page.md", BellowsSkillsWeb),
    tpl!("bellows/skills/migration.md.jinja" => ".bellows/skills/migration.md", BellowsSkills),
    tpl!("bellows/skills/mail.md.jinja" => ".bellows/skills/mail.md", BellowsSkills),
    tpl!("bellows/skills/agent.md.jinja" => ".bellows/skills/agent.md", BellowsSkillsAgents),
    tpl!("bellows/skills/api-token.md.jinja" => ".bellows/skills/api-token.md", BellowsSkillsHallmark),
    tpl!("bellows/skills/broadcast.md.jinja" => ".bellows/skills/broadcast.md", BellowsSkillsAnvil),
    tpl!("bellows/skills/search.md.jinja" => ".bellows/skills/search.md", BellowsSkillsSearch),
    tpl!("mcp.json" => ".mcp.json", BellowsMcp),
];

/// The values templates can use.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct Context {
    /// Package and binary name (`my-app`).
    pub(crate) name: String,
    /// Rust library name (`my_app`).
    pub(crate) lib_name: String,
    /// Display name (`My App`).
    pub(crate) title: String,
    pub(crate) web: bool,
    pub(crate) agents: bool,
    /// The authentication scaffolding (web apps only).
    pub(crate) auth: bool,
    /// Authentication through Temper: `.temper(…)`, `app/providers/temper.rs`, `app/actions/temper/`, the settings
    /// pages and the two-factor columns (web apps with the Temper block).
    pub(crate) temper: bool,
    /// Alpine.js in `public/assets/js/` and the layout (web apps only).
    pub(crate) alpine: bool,
    /// [`ALPINE_VERSION`], for the layout's `?v=` cache buster.
    pub(crate) alpine_version: String,
    pub(crate) database_url: String,
    /// The full `smeltery = …` dependency line.
    pub(crate) smeltery_dep: String,
    /// `APP_KEY` value; empty for `.env.example`.
    pub(crate) app_key: String,
    /// The Bellows parts chosen: `.mcp.json`, `.bellows/skills/`, `.bellows/guidelines.md`.
    pub(crate) bellows_mcp: bool,
    pub(crate) bellows_skills: bool,
    pub(crate) bellows_guidelines: bool,
    /// Module name of the users migration (`m2026_10_03_120000_create_users_table`).
    pub(crate) users_migration: String,
    /// Its `Migration::name` (`2026_10_03_120000_create_users_table`).
    pub(crate) users_migration_name: String,
    /// The password reset tokens migration (one second later), module and name.
    pub(crate) resets_migration: String,
    pub(crate) resets_migration_name: String,
    /// The sessions migration (two seconds later), module and name.
    pub(crate) sessions_migration: String,
    pub(crate) sessions_migration_name: String,
    /// The Watchfire tables migration (three seconds later), module and name; apps with agents only.
    pub(crate) watchfire_migration: String,
    pub(crate) watchfire_migration_name: String,
    /// The cache tables migration (four seconds later), module and name.
    pub(crate) cache_migration: String,
    pub(crate) cache_migration_name: String,
    /// The PubSub messages migration (five seconds later), module and name; apps with Watchfire only (D-507).
    pub(crate) pubsub_migration: String,
    pub(crate) pubsub_migration_name: String,
    /// The two-factor columns migration (six seconds later), module and name; apps with Temper only.
    pub(crate) two_factor_migration: String,
    pub(crate) two_factor_migration_name: String,
    /// Hallmark's API tokens (web apps with the Hallmark block): the migration, `.hallmark(…)`, `routes/api.rs`'s
    /// token routes, `app/controllers/api/`, `tests/api_tokens.rs`.
    pub(crate) hallmark: bool,
    /// Anvil's WebSockets and broadcasting (web apps with the Anvil block): `.anvil(…)`, `routes/channels.rs`,
    /// `app/events/`, `tests/broadcasting.rs`, the PubSub messages migration.
    pub(crate) anvil: bool,
    /// Prospect's search (web apps with the Prospect block): `.prospect(…)`, `app/providers/search.rs`,
    /// `PROSPECT_DRIVER`.
    pub(crate) search: bool,
    /// The presence tables migration (eight seconds later), module and name; React / Vue apps with Anvil and
    /// authentication (the dashboard's presence channel).
    pub(crate) presence: bool,
    pub(crate) presence_migration: String,
    pub(crate) presence_migration_name: String,
    /// The personal access tokens migration (seven seconds later), module and name; apps with Hallmark only.
    pub(crate) hallmark_migration: String,
    pub(crate) hallmark_migration_name: String,
    /// The frontend value of `--frontend` (`mold`, `react`, `vue`); empty for headless apps.
    pub(crate) frontend: String,
    /// A web app with the Mold frontend (Sparks, Mold pages).
    pub(crate) mold: bool,
    /// A web app with a JavaScript kit (React or Vue through Alloy).
    pub(crate) alloy: bool,
    pub(crate) react: bool,
    pub(crate) vue: bool,
    /// The kit's display name (`React`, `Vue`).
    pub(crate) kit_name: String,
    /// Tailwind: for the JavaScript kits, the npm packages and the Vite plugin (otherwise the prebuilt CSS).
    pub(crate) tailwind: bool,
    /// The Vite entry of a JavaScript kit (`resources/js/app.tsx`, `resources/js/app.ts`).
    pub(crate) entry: String,
    /// The extension of a JavaScript kit's page files (`tsx`, `vue`).
    pub(crate) page_ext: String,
    /// The component names of a JavaScript kit's pages.
    pub(crate) pages: Pages,
}

/// The Alloy component names of the kits' pages: lowercase paths in React (`auth/login`), PascalCase in Vue
/// (`auth/Login`) (D-283).
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct Pages {
    pub(crate) welcome: &'static str,
    pub(crate) dashboard: &'static str,
    pub(crate) login: &'static str,
    pub(crate) register: &'static str,
    pub(crate) forgot_password: &'static str,
    pub(crate) reset_password: &'static str,
    pub(crate) verify_email: &'static str,
    pub(crate) confirm_password: &'static str,
    pub(crate) two_factor_challenge: &'static str,
    pub(crate) settings_profile: &'static str,
    pub(crate) settings_password: &'static str,
    pub(crate) settings_two_factor: &'static str,
}

impl Pages {
    /// The React kit's names.
    pub(crate) const REACT: Pages = Pages {
        welcome: "welcome",
        dashboard: "dashboard",
        login: "auth/login",
        register: "auth/register",
        forgot_password: "auth/forgot-password",
        reset_password: "auth/reset-password",
        verify_email: "auth/verify-email",
        confirm_password: "auth/confirm-password",
        two_factor_challenge: "auth/two-factor-challenge",
        settings_profile: "settings/profile",
        settings_password: "settings/password",
        settings_two_factor: "settings/two-factor",
    };

    /// The Vue kit's names.
    pub(crate) const VUE: Pages = Pages {
        welcome: "Welcome",
        dashboard: "Dashboard",
        login: "auth/Login",
        register: "auth/Register",
        forgot_password: "auth/ForgotPassword",
        reset_password: "auth/ResetPassword",
        verify_email: "auth/VerifyEmail",
        confirm_password: "auth/ConfirmPassword",
        two_factor_challenge: "auth/TwoFactorChallenge",
        settings_profile: "settings/Profile",
        settings_password: "settings/Password",
        settings_two_factor: "settings/TwoFactor",
    };
}

impl Context {
    /// The values that follow from the starter kit (`None`: a headless app): `frontend`, `mold`, `alloy`, `react`,
    /// `vue`, `kit_name`, `entry`, `page_ext` and `pages`.
    pub(crate) fn with_frontend(mut self, frontend: Option<crate::new::Frontend>) -> Self {
        use crate::new::Frontend;
        let (kit_name, entry, page_ext, pages) = match frontend {
            Some(Frontend::React) => ("React", "resources/js/app.tsx", "tsx", Pages::REACT),
            Some(Frontend::Vue) => ("Vue", "resources/js/app.ts", "vue", Pages::VUE),
            _ => ("", "", "", Pages::default()),
        };
        self.frontend = frontend.map(crate::new::value_name).unwrap_or_default();
        self.mold = frontend == Some(Frontend::Mold);
        self.alloy = frontend.is_some_and(Frontend::is_js);
        self.react = frontend == Some(Frontend::React);
        self.vue = frontend == Some(Frontend::Vue);
        self.kit_name = kit_name.to_owned();
        self.entry = entry.to_owned();
        self.page_ext = page_ext.to_owned();
        self.pages = pages;
        self
    }
}

/// The output path of `template`, which may use context values (`{{ users_migration }}`).
pub(crate) fn out_path(template: &Template, ctx: &Context) -> anyhow::Result<String> {
    if !template.out.contains("{{") {
        return Ok(template.out.to_owned());
    }
    environment()
        .render_str(template.out, ctx)
        .map_err(|e| anyhow::anyhow!("output path {} failed: {e:#}", template.out))
}

fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_keep_trailing_newline(true);
    // Output is Rust, TOML and Markdown: never HTML-escape it.
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env
}

/// Renders a template `source` named `name` with any serializable context.
pub(crate) fn render_source(
    name: &str,
    source: &str,
    ctx: impl Serialize,
) -> anyhow::Result<String> {
    environment()
        .render_named_str(name, source, ctx)
        .map_err(|e| anyhow::anyhow!("template {name} failed: {e:#}"))
}

/// Renders `template` with `ctx` (or returns it verbatim when it is not a `.jinja` source).
pub(crate) fn render(template: &Template, ctx: &Context) -> anyhow::Result<String> {
    if !template.is_rendered() {
        return Ok(template.source.to_owned());
    }
    let env = environment();
    let mut ctx = ctx.clone();
    if template.out == ".env.example" {
        ctx.app_key = String::new();
    }
    env.render_named_str(template.src, template.source, ctx)
        .map_err(|e| anyhow::anyhow!("template {} failed: {e:#}", template.src))
}
