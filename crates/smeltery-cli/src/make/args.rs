//! Command-line arguments of the `make:*` commands.

use clap::Args;

/// `make:model Name [field:type ...]`.
#[derive(Debug, Args)]
pub(crate) struct ModelArgs {
    /// Model name, e.g. `Post` or `post_comment`.
    pub(crate) name: String,
    /// Fields as `name:type`, `?` for nullable: `title:string body:text? user_id:foreign image:file`.
    pub(crate) fields: Vec<String>,
    /// Also create a migration for the table.
    #[arg(short = 'm', long)]
    pub(crate) migration: bool,
    /// Also create a controller with an index page (a Mold view, or a React / Vue page).
    #[arg(short = 'c', long)]
    pub(crate) controller: bool,
    /// Also create a resource controller, its views (React / Vue apps: pages) and its routes.
    #[arg(short = 'r', long)]
    pub(crate) resource: bool,
    /// Also create a factory.
    #[arg(short = 'f', long)]
    pub(crate) factory: bool,
    /// Also create a seeder.
    #[arg(short = 's', long)]
    pub(crate) seeder: bool,
    /// Migration, resource controller, factory and seeder.
    #[arg(long)]
    pub(crate) all: bool,
    /// Make the model searchable (an app with the Prospect building block): `impl Searchable`, its search index in a
    /// migration, the registration in `app/providers/search.rs`; with `-r` the list page searches.
    #[arg(long)]
    pub(crate) searchable: bool,
}

/// `make:controller Name`.
#[derive(Debug, Args)]
pub(crate) struct ControllerArgs {
    /// Controller name, e.g. `Dashboard`; with `--resource`, the model name (`Post`).
    pub(crate) name: String,
    /// Create the seven resource actions, their views (React / Vue apps: pages) and routes.
    #[arg(short = 'r', long)]
    pub(crate) resource: bool,
    /// The model of a resource controller (default: the controller name).
    #[arg(long)]
    pub(crate) model: Option<String>,
}

/// `make:migration name`.
#[derive(Debug, Args)]
pub(crate) struct MigrationArgs {
    /// Migration name, e.g. `create_posts_table` or `add_slug_to_posts_table`.
    pub(crate) name: String,
}

/// `make:middleware Name`.
#[derive(Debug, Args)]
pub(crate) struct MiddlewareArgs {
    /// Middleware name, e.g. `EnsureAdmin`.
    pub(crate) name: String,
}

/// `make:seeder Name`.
#[derive(Debug, Args)]
pub(crate) struct SeederArgs {
    /// Seeder name, e.g. `PostSeeder` or `Post`.
    pub(crate) name: String,
}

/// `make:factory Name`.
#[derive(Debug, Args)]
pub(crate) struct FactoryArgs {
    /// Factory name, e.g. `PostFactory` or `Post`.
    pub(crate) name: String,
    /// The model it builds (default: the factory name without `Factory`).
    #[arg(long)]
    pub(crate) model: Option<String>,
}

/// `make:command Name`.
#[derive(Debug, Args)]
pub(crate) struct CommandArgs {
    /// Command name, e.g. `SendReport` (run as `smeltery send-report`).
    pub(crate) name: String,
}

/// `make:agent Name`.
#[derive(Debug, Args)]
pub(crate) struct AgentArgs {
    /// Agent name, e.g. `PricePoller`.
    pub(crate) name: String,
}

/// `make:job Name`.
#[derive(Debug, Args)]
pub(crate) struct JobArgs {
    /// Job name, e.g. `SendWelcome`.
    pub(crate) name: String,
}

/// `make:spark Name`.
#[derive(Debug, Args)]
pub(crate) struct SparkArgs {
    /// Spark name, e.g. `TodoList`.
    pub(crate) name: String,
    /// Make it listen to broadcasts (an app with the Anvil building block): the full channel name, e.g.
    /// `announcements` or `private-orders.{order_id}` (a `{field}` becomes a field of the Spark).
    #[arg(long, value_name = "CHANNEL", requires = "event")]
    pub(crate) listen: Option<String>,
    /// The event the listener takes: an event type (`OrderShipped`, sent as `App\Events\OrderShipped`) or the
    /// name an event is sent with (`order.shipped`).
    #[arg(long, value_name = "EVENT", requires = "listen")]
    pub(crate) event: Option<String>,
}

/// `make:page Name` (React / Vue apps).
#[derive(Debug, Args)]
pub(crate) struct PageArgs {
    /// Page name, e.g. `About` (`resources/js/pages/about.tsx` or `About.vue`, served at `/about`).
    pub(crate) name: String,
}

/// `make:mail Name`.
#[derive(Debug, Args)]
pub(crate) struct MailArgs {
    /// Mail name, e.g. `Welcome` or `InvoicePaid`.
    pub(crate) name: String,
}
