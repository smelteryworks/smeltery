//! The generators: each turns its arguments into a [`Plan`].

use anyhow::{Context as _, bail};
use serde::Serialize;

use super::args::{
    AgentArgs, CommandArgs, ControllerArgs, FactoryArgs, JobArgs, MailArgs, MiddlewareArgs,
    MigrationArgs, ModelArgs, PageArgs, SeederArgs, SparkArgs,
};
use super::fields::{Field, FieldType};
use super::names::Name;
use super::{Ctx, Plan, stamp};
use crate::new::Frontend;
use crate::templates::render_source;

macro_rules! make_template {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/templates/make/",
                $name
            )),
        )
    };
}

const MODEL: (&str, &str) = make_template!("model.rs.jinja");
const MIGRATION: (&str, &str) = make_template!("migration.rs.jinja");
const FACTORY: (&str, &str) = make_template!("factory.rs.jinja");
const SEEDER: (&str, &str) = make_template!("seeder.rs.jinja");
const COMMAND: (&str, &str) = make_template!("command.rs.jinja");
const AGENT: (&str, &str) = make_template!("agent.rs.jinja");
const SPARK: (&str, &str) = make_template!("spark.rs.jinja");
const MAIL: (&str, &str) = make_template!("mail.rs.jinja");
const MAIL_VIEW: (&str, &str) = make_template!("mail.mold.html.jinja");
const SPARK_VIEW: (&str, &str) = make_template!("spark.mold.html.jinja");
const SPARK_LISTENER: (&str, &str) = make_template!("spark_listener.rs.jinja");
const SPARK_LISTENER_VIEW: (&str, &str) = make_template!("spark_listener.mold.html.jinja");
const JOB: (&str, &str) = make_template!("job.rs.jinja");
const MIDDLEWARE: (&str, &str) = make_template!("middleware.rs.jinja");
const CONTROLLER: (&str, &str) = make_template!("controller.rs.jinja");
const VIEW: (&str, &str) = make_template!("view.mold.html.jinja");
const RESOURCE: (&str, &str) = make_template!("resource.rs.jinja");
const RESOURCE_INDEX: (&str, &str) = make_template!("resource_index.mold.html.jinja");
const RESOURCE_SHOW: (&str, &str) = make_template!("resource_show.mold.html.jinja");
const RESOURCE_FORM: (&str, &str) = make_template!("resource_form.mold.html.jinja");
// The React and Vue kits' pages (Alloy).
const TYPES: (&str, &str) = make_template!("types.ts.jinja");
const REACT_INDEX: (&str, &str) = make_template!("react/index.tsx.jinja");
const REACT_FORM: (&str, &str) = make_template!("react/form.tsx.jinja");
const REACT_SHOW: (&str, &str) = make_template!("react/show.tsx.jinja");
const REACT_PAGE: (&str, &str) = make_template!("react/page.tsx.jinja");
const VUE_INDEX: (&str, &str) = make_template!("vue/Index.vue.jinja");
const VUE_FORM: (&str, &str) = make_template!("vue/Form.vue.jinja");
const VUE_SHOW: (&str, &str) = make_template!("vue/Show.vue.jinja");
const VUE_PAGE: (&str, &str) = make_template!("vue/Page.vue.jinja");
const SEARCH_TEST: (&str, &str) = make_template!("search_test.rs.jinja");

fn render(template: (&str, &str), ctx: impl Serialize) -> anyhow::Result<String> {
    render_source(template.0, template.1, ctx)
}

// Marker comments in a generated app.
const MODS: &str = "// smeltery:mods";
const MODELS: &str = "// smeltery:models";
const MIGRATIONS: &str = "// smeltery:migrations";
const SEEDERS: &str = "// smeltery:seeders";
const COMMANDS: &str = "// smeltery:commands";
const ROUTES: &str = "// smeltery:routes";
const AGENTS: &str = "// smeltery:agents";
const SPARKS: &str = "// smeltery:sparks";
const SEARCHABLES: &str = "// smeltery:searchables";

/// A Mold echo, `{{ expr }}`.
fn echo(expr: &str) -> String {
    format!("{{{{ {expr} }}}}")
}

/// Everything the model-based templates need.
#[derive(Debug, Serialize)]
struct ModelCtx {
    pascal: String,
    snake: String,
    /// Table name, also the controller module and the list variable: `post_comments`.
    table: String,
    /// Views folder: `post_comments`.
    views: String,
    /// URL base: `/post-comments`.
    url: String,
    /// Route name prefix: `post-comments`.
    route: String,
    title: String,
    plural_title: String,
    lower: String,
    plural_lower: String,
    id_echo: String,
    label_echo: String,
    /// `{{ message }}`, the error text inside `@error`.
    message_echo: String,
    fields: Vec<FieldCtx>,
    form_fields: Vec<FieldCtx>,
    /// Some form field is a file: the forms post `multipart/form-data`.
    has_files: bool,
    /// A required file field: the edit form gets its own struct, where the file is optional.
    update_form: bool,
    /// Where uploads are stored, under `storage/app`: `public/post_comments`.
    uploads: String,
    /// The model is searchable (Prospect): what its `impl Searchable`, its index and the list page need.
    search: Option<SearchCtx>,
}

/// A searchable model (`--searchable`, or an existing model with `impl Searchable`).
#[derive(Debug, Clone, Serialize)]
struct SearchCtx {
    /// The searched columns in index order: the first has `Weight::A`, the others the default `Weight::B`.
    texts: Vec<SearchColumn>,
    /// The `foreign` fields: filter columns.
    filters: Vec<String>,
    /// The column the list page highlights (its label column), when the label is a searched column.
    highlight: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SearchColumn {
    name: String,
    weight: &'static str,
}

impl SearchCtx {
    /// From the fields of a new model: every `string` / `text` field is searched, every `foreign` field filters.
    fn from_fields(fields: &[Field], label: Option<&str>) -> Option<Self> {
        let names: Vec<String> = fields
            .iter()
            .filter(|f| matches!(f.ty, FieldType::String | FieldType::Text))
            .map(|f| f.name.clone())
            .collect();
        let filters = fields
            .iter()
            .filter(|f| f.ty == FieldType::Foreign)
            .map(|f| f.name.clone())
            .collect();
        Self::new(names, filters, label)
    }

    /// From the source of an existing model: its `i.text("…")` / `i.filter("…")` lines, `None` when it does not
    /// implement `Searchable`.
    fn from_model_source(source: &str, label: Option<&str>) -> Option<Self> {
        if !source.contains("impl Searchable for Model") {
            return None;
        }
        let arg = |line: &str, call: &str| {
            line.trim()
                .strip_prefix(call)
                .and_then(|rest| rest.split('"').next())
                .map(str::to_owned)
        };
        let names = source.lines().filter_map(|l| arg(l, "i.text(\"")).collect();
        let filters = source
            .lines()
            .filter_map(|l| arg(l, "i.filter(\""))
            .collect();
        Self::new(names, filters, label)
    }

    fn new(names: Vec<String>, filters: Vec<String>, label: Option<&str>) -> Option<Self> {
        if names.is_empty() {
            return None;
        }
        let highlight = label
            .filter(|l| names.iter().any(|n| n == l))
            .map(str::to_owned);
        let texts = names
            .into_iter()
            .enumerate()
            .map(|(i, name)| SearchColumn {
                name,
                weight: if i == 0 { "A" } else { "B" },
            })
            .collect();
        Some(SearchCtx {
            texts,
            filters,
            highlight,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
struct FieldCtx {
    name: String,
    label: String,
    rust_type: String,
    blueprint: String,
    fake: String,
    checkbox: bool,
    textarea: bool,
    /// A `file` field: an `UploadedFile` in the form, the stored path in the model.
    file: bool,
    nullable: bool,
    /// The doc line above the model field, if any.
    doc: String,
    /// The type in the form struct (`Option<UploadedFile>` for a file).
    form_type: String,
    /// Extra `<input>` / `<textarea>` attributes, each with a leading space.
    attrs: String,
    /// `post.title`.
    path: String,
    /// `{{ post.title }}`.
    echo: String,
    /// `{{ old("title") }}`: the value from a failed submit.
    old: String,
    /// `{{ old("title") | default(post.title) }}`: the failed submit's value, else the record's.
    old_or_value: String,
    /// The `#[validate(...)]` rules, e.g. `required, max = 255`; empty for none.
    rules: String,
    /// The rules in the edit form (a file is optional there: the stored one stays).
    update_rules: String,
    // The React / Vue pages (Alloy kits).
    /// The TypeScript type of the record's field as the browser gets it (`string | null`).
    ts_type: String,
    /// A number input (`integer`, `bigint`, `float`, `foreign`): the form holds text, sent as a number.
    number: bool,
    /// A `float` field: `step="any"`.
    step_any: bool,
    /// The browser's `required` attribute in the create form.
    required: bool,
    /// The field's start value in the create form (`''`, `false`, `null as File | null`).
    create_value: String,
    /// The start value in the edit form, from the record: `post.title`, `String(post.views)`.
    edit_value: String,
    /// The same in a Vue `<script setup>`, where the record is `props.post`.
    edit_value_vue: String,
    /// The value shown on the record's page, a TypeScript expression: `post.title`, `post.published ? 'Yes' : 'No'`.
    show_expr: String,
    /// `{{ <show_expr> }}` for Vue templates.
    vue_show: String,
    /// `{{ form.errors.title }}` for Vue templates.
    vue_error: String,
}

/// What the React / Vue pages of a resource need besides [`ModelCtx`].
#[derive(Debug, Serialize)]
struct KitCtx {
    react: bool,
    vue: bool,
    /// The pages' folder under `resources/js/pages/`: `post-comments`.
    dir: String,
    /// `tsx` or `vue`.
    ext: &'static str,
    /// The component names `alloy::render` uses: `post-comments/index` (React), `post-comments/Index` (Vue).
    index: String,
    create: String,
    show: String,
    edit: String,
    /// The page files, relative to the app: `resources/js/pages/post-comments/index.tsx`.
    index_file: String,
    create_file: String,
    show_file: String,
    edit_file: String,
    /// The TypeScript type of a record (`PostComment`) and its module (`resources/js/types/post-comment.ts`).
    type_name: String,
    type_module: String,
    /// TypeScript variables: one record (`postComment`), the list (`postComments`).
    var: String,
    list_var: String,
    /// The list entry's text in React (`{post.title}`, `#{post.id}`) and in Vue (`{{ post.title }}`).
    label: String,
    vue_label: String,
    /// `{{ post.id }}` for Vue templates.
    vue_id: String,
    /// Number fields of the forms: sent as numbers in JSON.
    numbers: Vec<String>,
    /// Checkbox fields of the forms: sent as `true` / `false` text in a multipart form.
    bools: Vec<String>,
}

impl KitCtx {
    fn new(frontend: Frontend, name: &Name, m: &ModelCtx) -> Self {
        let react = frontend == Frontend::React;
        let dir = name.plural().kebab();
        let (ext, pages) = if react {
            ("tsx", ["index", "create", "show", "edit"])
        } else {
            ("vue", ["Index", "Create", "Show", "Edit"])
        };
        let [index, create, show, edit] = pages.map(|p| format!("{dir}/{p}"));
        let file = |component: &str| format!("resources/js/pages/{component}.{ext}");
        let var = name.camel();
        let label_field = m
            .form_fields
            .iter()
            .find(|f| f.rust_type == "String" && !f.file);
        let (label, vue_label) = match label_field {
            Some(f) => (
                format!("{{{var}.{}}}", f.name),
                echo(&format!("{var}.{}", f.name)),
            ),
            None => (
                format!("#{{{var}.id}}"),
                format!("#{}", echo(&format!("{var}.id"))),
            ),
        };
        KitCtx {
            react,
            vue: !react,
            ext,
            index_file: file(&index),
            create_file: file(&create),
            show_file: file(&show),
            edit_file: file(&edit),
            index,
            create,
            show,
            edit,
            type_name: name.pascal(),
            type_module: name.kebab(),
            list_var: name.plural().camel(),
            label,
            vue_label,
            vue_id: echo(&format!("{var}.id")),
            numbers: m
                .form_fields
                .iter()
                .filter(|f| f.number)
                .map(|f| f.name.clone())
                .collect(),
            bools: m
                .form_fields
                .iter()
                .filter(|f| f.checkbox)
                .map(|f| f.name.clone())
                .collect(),
            var,
            dir,
        }
    }
}

impl ModelCtx {
    fn new(name: &Name, fields: &[Field]) -> Self {
        let plural = name.plural();
        let snake = name.snake();
        let var = name.camel();
        let form_fields: Vec<FieldCtx> = fields
            .iter()
            .filter(|f| f.ty.in_forms())
            .map(|f| field_ctx(&snake, &var, f))
            .collect();
        let fields: Vec<FieldCtx> = fields.iter().map(|f| field_ctx(&snake, &var, f)).collect();
        let label_echo = form_fields
            .iter()
            .find(|f| f.rust_type == "String" && !f.file)
            .map(|f| f.echo.clone())
            .unwrap_or_else(|| format!("#{}", echo(&format!("{snake}.id"))));
        ModelCtx {
            pascal: name.pascal(),
            table: plural.snake(),
            views: plural.snake(),
            url: format!("/{}", plural.kebab()),
            route: plural.kebab(),
            title: name.sentence(),
            plural_title: plural.sentence(),
            lower: name.words.join(" "),
            plural_lower: plural.words.join(" "),
            id_echo: echo(&format!("{snake}.id")),
            label_echo,
            message_echo: echo("message"),
            has_files: form_fields.iter().any(|f| f.file),
            update_form: form_fields
                .iter()
                .any(|f| f.file && f.rules != f.update_rules),
            uploads: format!("public/{}", plural.snake()),
            search: None,
            snake,
            fields,
            form_fields,
        }
    }

    /// The name of the label field (the first non-file `String` form field), if any.
    fn label_field(&self) -> Option<&str> {
        self.form_fields
            .iter()
            .find(|f| f.rust_type == "String" && !f.file)
            .map(|f| f.name.as_str())
    }
}

/// The `mimes` rule of a `file` field whose name says it holds an image (`image`, `photo`, `avatar`, `cover_image`,
/// ...): images only.
pub(crate) const IMAGE_MIMES: &str = "mimes = \"jpg,jpeg,png,gif,webp\"";

/// The `mimes` rule of any other `file` field: images, PDF and office or text documents. Never a type a browser runs
/// as a page on the app's origin (`html`, `svg`, `xml`, `js`): uploads are served from `/storage/…` (S6-01, D-350).
pub(crate) const FILE_MIMES: &str = "mimes = \"jpg,jpeg,png,gif,webp,pdf,txt,csv,docx,xlsx\"";

/// Words that make a `file` field an image field, as the whole name or one `_` part of it.
const IMAGE_WORDS: &[&str] = &[
    "image",
    "img",
    "photo",
    "picture",
    "pic",
    "avatar",
    "logo",
    "thumbnail",
    "thumb",
    "icon",
    "cover",
    "banner",
    "poster",
];

/// The default type allow-list of a `file` field (see [`IMAGE_MIMES`], [`FILE_MIMES`]).
fn file_mimes(name: &str) -> &'static str {
    if name.split('_').any(|part| IMAGE_WORDS.contains(&part)) {
        IMAGE_MIMES
    } else {
        FILE_MIMES
    }
}

fn field_ctx(model: &str, var: &str, f: &Field) -> FieldCtx {
    let path = format!("{model}.{}", f.name);
    let number = matches!(
        f.ty,
        FieldType::Integer | FieldType::Bigint | FieldType::Float | FieldType::Foreign
    );
    let ts = match f.ty {
        FieldType::Integer | FieldType::Bigint | FieldType::Float | FieldType::Foreign => "number",
        FieldType::Bool => "boolean",
        FieldType::Json => "unknown",
        _ => "string",
    };
    let ts_type = if f.nullable && f.ty != FieldType::Json {
        format!("{ts} | null")
    } else {
        ts.to_owned()
    };
    let (create_value, edit_value) = match f.ty {
        FieldType::Bool => ("false".to_owned(), format!("{var}.{}", f.name)),
        FieldType::File => (
            "null as File | null".to_owned(),
            "null as File | null".to_owned(),
        ),
        _ if number && f.nullable => (
            "''".to_owned(),
            format!("{var}.{0} === null ? '' : String({var}.{0})", f.name),
        ),
        _ if number => ("''".to_owned(), format!("String({var}.{})", f.name)),
        _ if f.nullable => ("''".to_owned(), format!("{var}.{} ?? ''", f.name)),
        _ => ("''".to_owned(), format!("{var}.{}", f.name)),
    };
    let edit_value_vue = if f.ty == FieldType::File {
        edit_value.clone()
    } else {
        edit_value.replace(&format!("{var}."), &format!("props.{var}."))
    };
    let show_expr = if f.ty == FieldType::Bool {
        format!("{var}.{} ? 'Yes' : 'No'", f.name)
    } else {
        format!("{var}.{}", f.name)
    };
    let required = if f.nullable { "" } else { " required" };
    let attrs = match f.ty {
        FieldType::String => format!(" type=\"text\"{required}"),
        FieldType::Text => required.to_owned(),
        FieldType::Integer | FieldType::Bigint | FieldType::Foreign => {
            format!(" type=\"number\"{required}")
        }
        FieldType::Float => format!(" type=\"number\" step=\"any\"{required}"),
        FieldType::File => format!(" type=\"file\"{required}"),
        _ => String::new(),
    };
    let mut rules = Vec::new();
    if !f.nullable && f.ty != FieldType::Bool {
        rules.push("required");
    }
    match f.ty {
        FieldType::String => rules.push("max = 255"),
        FieldType::Integer | FieldType::Bigint | FieldType::Foreign => rules.push("integer"),
        FieldType::Float => rules.push("numeric"),
        FieldType::File => {
            rules.push("max = 2048");
            rules.push(file_mimes(&f.name));
        }
        _ => {}
    }
    let file = f.ty == FieldType::File;
    let update_rules = if file {
        rules
            .iter()
            .filter(|r| **r != "required")
            .copied()
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        rules.join(", ")
    };
    FieldCtx {
        file,
        nullable: f.nullable,
        doc: if file {
            super::fields::FILE_DOC.to_owned()
        } else {
            String::new()
        },
        form_type: if file {
            "Option<UploadedFile>".to_owned()
        } else {
            f.rust_type()
        },
        update_rules,
        old: echo(&format!("old(\"{}\")", f.name)),
        old_or_value: echo(&format!("old(\"{}\") | default({path})", f.name)),
        rules: rules.join(", "),
        name: f.name.clone(),
        label: Name::parse(&f.name)
            .map(|n| n.sentence())
            .unwrap_or_else(|_| f.name.clone()),
        rust_type: f.rust_type(),
        blueprint: f.blueprint(),
        fake: f.fake(),
        checkbox: f.ty == FieldType::Bool,
        textarea: f.ty == FieldType::Text,
        attrs,
        echo: echo(&path),
        path,
        ts_type,
        number,
        step_any: f.ty == FieldType::Float,
        required: !f.nullable && f.ty != FieldType::Bool,
        create_value,
        edit_value,
        edit_value_vue,
        vue_show: echo(&show_expr),
        show_expr,
        vue_error: echo(&format!("form.errors.{}", f.name)),
    }
}

fn skipped_note(fields: &[Field], controller: &str) -> Option<String> {
    let skipped: Vec<&str> = fields
        .iter()
        .filter(|f| !f.ty.in_forms())
        .map(|f| f.name.as_str())
        .collect();
    (!skipped.is_empty()).then(|| {
        format!(
            "note: {} not in the forms and views; set {} in app/controllers/{controller}.rs",
            skipped.join(", "),
            if skipped.len() == 1 { "it" } else { "them" }
        )
    })
}

/// `make:model`.
pub(crate) fn model(ctx: Ctx<'_>, args: &ModelArgs) -> anyhow::Result<Plan> {
    let name = Name::parse(&args.name)?;
    let fields = Field::parse_all(&args.fields)?;
    let mut m = ModelCtx::new(&name, &fields);
    if args.searchable {
        if !has_search(ctx) {
            bail!(
                "this app has no Prospect building block (no `.prospect(` in bootstrap/app.rs); add it with \
                 `smeltery prospect:install` first; nothing was changed"
            );
        }
        let label = m.label_field().map(str::to_owned);
        m.search = SearchCtx::from_fields(&fields, label.as_deref());
        if m.search.is_none() {
            bail!(
                "--searchable needs a `string` or `text` field to search (e.g. `title:string`); nothing was \
                 changed"
            );
        }
    }
    let mut plan = Plan::default();
    plan.file(
        format!("app/models/{}.rs", m.snake),
        render(MODEL, minijinja_ctx(&m))?,
    );
    plan.insert("app/models/mod.rs", MODS, format!("pub mod {};", m.snake));
    plan.insert(
        "app/models/mod.rs",
        MODELS,
        format!("pub use {}::Model as {};", m.snake, m.pascal),
    );
    let (migration, resource, factory, seeder) = (
        args.migration || args.all,
        args.resource || args.all,
        args.factory || args.all,
        args.seeder || args.all,
    );
    if migration {
        let snake = format!("create_{}_table", m.table);
        plan.extend(migration_plan(ctx, &snake, &fields, m.search.as_ref())?);
    } else if let Some(search) = &m.search {
        let snake = format!("add_search_index_to_{}_table", m.table);
        plan.extend(search_migration_plan(ctx, &snake, &m.table, search)?);
    }
    if m.search.is_some() {
        plan.insert(
            "app/providers/search.rs",
            SEARCHABLES,
            format!("p.model::<crate::app::models::{}>();", m.pascal),
        );
    }
    if (resource || args.controller) && !has_web(ctx) {
        plan.notes
            .push(format!("note: {NO_WEB}; the controller part was skipped"));
    } else if resource {
        plan.extend(resource_plan(ctx, &name, &m)?);
        plan.notes.extend(skipped_note(&fields, &m.table));
    } else if args.controller {
        plan.extend(plain_controller_plan(ctx, &name.plural())?);
    }
    if factory {
        plan.extend(factory_plan(&m)?);
    }
    // A searchable list with a factory to fill it: a test of the search, without and with a search text.
    if let (true, true, Some(hl)) = (
        resource && has_web(ctx),
        factory,
        m.search.as_ref().and_then(|s| s.highlight.clone()),
    ) {
        plan.extend(search_test_plan(ctx, &name, &m, &hl)?);
    }
    if seeder {
        plan.extend(seeder_plan(&name, factory.then_some(&m))?);
    }
    Ok(plan)
}

fn minijinja_ctx(m: &ModelCtx) -> impl Serialize + '_ {
    #[derive(Serialize)]
    struct C<'a> {
        m: &'a ModelCtx,
    }
    C { m }
}

/// Reads the fields of an existing model, for generators that need them (`make:controller --resource`,
/// `make:factory`).
fn existing_model(ctx: Ctx<'_>, name: &Name) -> anyhow::Result<Vec<Field>> {
    let rel = format!("app/models/{}.rs", name.snake());
    let source = std::fs::read_to_string(ctx.root.join(&rel)).with_context(|| {
        format!(
            "{rel} not found; create the model first: smeltery make:model {}",
            name.pascal()
        )
    })?;
    Ok(Field::from_model_source(&source))
}

/// Why an app without `routes/web.rs` gets no controller.
const NO_WEB: &str = "this app has no web routes (no routes/web.rs, as in a headless app), so it takes no controllers";

/// Whether the app has web routes (a headless app has no `routes/` directory).
fn has_web(ctx: Ctx<'_>) -> bool {
    ctx.root.join("routes").join("web.rs").is_file()
}

/// `make:controller`.
pub(crate) fn controller(ctx: Ctx<'_>, args: &ControllerArgs) -> anyhow::Result<Plan> {
    if !has_web(ctx) {
        bail!("{NO_WEB}; nothing was changed");
    }
    let name = Name::parse(&args.name)?.without_suffix("controller");
    if !args.resource {
        if args.model.is_some() {
            bail!("--model needs --resource");
        }
        return plain_controller_plan(ctx, &name);
    }
    let model = match &args.model {
        Some(m) => Name::parse(m)?,
        None => name,
    };
    let fields = existing_model(ctx, &model)?;
    let mut m = ModelCtx::new(&model, &fields);
    // A searchable model's list page searches (`impl Searchable` in its file).
    let label = m.label_field().map(str::to_owned);
    let source = std::fs::read_to_string(ctx.root.join(format!("app/models/{}.rs", model.snake())))
        .unwrap_or_default();
    m.search = SearchCtx::from_model_source(&source, label.as_deref()).filter(|_| has_search(ctx));
    // A scoped model's search needs the scope value (whose team, whose account), which only the app knows: a
    // generated list would either fail every search or have to search across scopes. Refused (Stage E review M1).
    if m.search.is_some() && source.contains("i.scoped_by(") {
        bail!(
            "app/models/{}.rs scopes its search (`i.scoped_by(…)`): the list needs the scope value of the \
             signed-in user, so write its `index` by hand with `.within(value)` (see the Search section of \
             CLAUDE.md); nothing was changed",
            model.snake()
        );
    }
    let mut plan = resource_plan(ctx, &model, &m)?;
    plan.notes.extend(skipped_note(&fields, &m.table));
    Ok(plan)
}

/// The app's starter kit (`[package.metadata.smeltery] frontend` in its `Cargo.toml`; Mold when absent).
fn app_frontend(ctx: Ctx<'_>) -> anyhow::Result<Frontend> {
    crate::frontend::app_frontend(ctx.root)
}

/// What a plain controller (`make:controller Name`) and `make:page Name` write.
#[derive(Serialize)]
struct PageCtx {
    module: String,
    views: String,
    title: String,
    /// React / Vue: the component name, its file, the page's function name and the route.
    alloy: bool,
    react: bool,
    component: String,
    file: String,
    function: String,
    handler: String,
    url: String,
}

fn plain_controller_plan(ctx: Ctx<'_>, name: &Name) -> anyhow::Result<Plan> {
    let frontend = app_frontend(ctx)?;
    let react = frontend == Frontend::React;
    let component = match frontend {
        Frontend::React => format!("{}/index", name.kebab()),
        _ => format!("{}/Index", name.kebab()),
    };
    let c = PageCtx {
        module: name.snake(),
        views: name.snake(),
        title: name.sentence(),
        alloy: frontend.is_js(),
        react,
        file: format!(
            "resources/js/pages/{component}.{}",
            if react { "tsx" } else { "vue" }
        ),
        component,
        function: "Index".to_owned(),
        handler: "index".to_owned(),
        url: format!("/{}", name.kebab()),
    };
    let mut plan = Plan::default();
    plan.file(
        format!("app/controllers/{}.rs", c.module),
        render(CONTROLLER, &c)?,
    );
    if c.alloy {
        plan.file(
            &c.file,
            render(if react { REACT_PAGE } else { VUE_PAGE }, &c)?,
        );
    } else {
        plan.file(
            format!("resources/views/{}/index.mold.html", c.views),
            render(VIEW, &c)?,
        );
    }
    plan.insert(
        "app/controllers/mod.rs",
        MODS,
        format!("pub mod {};", c.module),
    );
    plan.insert(
        "routes/web.rs",
        ROUTES,
        get_route(
            &c.url,
            &c.module,
            "index",
            &format!("{}.index", name.kebab()),
        ),
    );
    Ok(plan)
}

fn resource_plan(ctx: Ctx<'_>, name: &Name, m: &ModelCtx) -> anyhow::Result<Plan> {
    #[derive(Serialize)]
    struct Form<'a> {
        m: &'a ModelCtx,
        k: Option<&'a KitCtx>,
        edit: bool,
        /// The app has authentication: the routes that change records need a signed-in user.
        auth: bool,
    }
    let frontend = app_frontend(ctx)?;
    let kit = frontend.is_js().then(|| KitCtx::new(frontend, name, m));
    let k = kit.as_ref();
    let auth = has_auth(ctx);
    let mut plan = Plan::default();
    plan.file(
        format!("app/controllers/{}.rs", m.table),
        render(
            RESOURCE,
            Form {
                m,
                k,
                edit: false,
                auth,
            },
        )?,
    );
    if let Some(k) = k {
        let (index, form, show) = if k.react {
            (REACT_INDEX, REACT_FORM, REACT_SHOW)
        } else {
            (VUE_INDEX, VUE_FORM, VUE_SHOW)
        };
        let page = |edit| Form {
            m,
            k: Some(k),
            edit,
            auth,
        };
        plan.file(&k.index_file, render(index, page(false))?);
        plan.file(&k.create_file, render(form, page(false))?);
        plan.file(&k.show_file, render(show, page(false))?);
        plan.file(&k.edit_file, render(form, page(true))?);
        plan.file(
            format!("resources/js/types/{}.ts", k.type_module),
            render(TYPES, page(false))?,
        );
    } else {
        let views = format!("resources/views/{}", m.views);
        plan.file(
            format!("{views}/index.mold.html"),
            render(RESOURCE_INDEX, minijinja_ctx(m))?,
        );
        let form = |edit| Form { m, k, edit, auth };
        plan.file(
            format!("{views}/create.mold.html"),
            render(RESOURCE_FORM, form(false))?,
        );
        plan.file(
            format!("{views}/show.mold.html"),
            render(RESOURCE_SHOW, minijinja_ctx(m))?,
        );
        plan.file(
            format!("{views}/edit.mold.html"),
            render(RESOURCE_FORM, form(true))?,
        );
    }
    plan.insert(
        "app/controllers/mod.rs",
        MODS,
        format!("pub mod {};", m.table),
    );
    plan.insert(
        "routes/web.rs",
        ROUTES,
        resource_routes(m, auth, m.search.is_some()),
    );
    plan.notes.push(if auth {
        format!(
            "note: in routes/web.rs, the {} routes that create, edit and delete records need a signed-in user \
             (`auth`); the list and the record pages are public",
            m.url
        )
    } else {
        format!(
            "note: the {} routes are public: anyone can create, edit and delete records; protect them in \
             routes/web.rs before deploying",
            m.url
        )
    });
    Ok(plan)
}

/// The `routes/web.rs` block of a resource (S6-02, D-351). In an app with authentication, `index` and `show` are
/// public and the actions that change records sit behind `auth`; without it, all seven are public, and the comment
/// above them says so. The comment names the URL, so the block's first line is unique (the insert is idempotent).
fn resource_routes(m: &ModelCtx, auth: bool, search: bool) -> String {
    let route = |action: &str| {
        format!(
            "\n    .{action}(crate::app::controllers::{}::{action})",
            m.table
        )
    };
    let mut block = String::new();
    if search {
        // The list searches: it gets its own registration with a throttle (a search ranks every match).
        let (rest, public): (&[&str], bool) = if auth {
            (&["create", "store", "edit", "update", "destroy"], false)
        } else {
            (
                &["create", "store", "show", "edit", "update", "destroy"],
                true,
            )
        };
        if auth {
            block.push_str(&format!(
                "// {}: viewing and searching are public (searches are throttled); creating, editing\n\
                 // and deleting need a signed-in user.\n",
                m.url
            ));
        } else {
            block.push_str(&format!(
                "// {}: public, anyone can search, create, edit and delete records (searches are\n\
                 // throttled).\n",
                m.url
            ));
        }
        block.push_str(&format!("r.resource(\"{}\")", m.url));
        block.push_str(&route("index"));
        block.push_str("\n    .middleware(\"throttle:60,1\");");
        if !public {
            block.push_str(&format!("\nr.resource(\"{}\")", m.url));
            block.push_str(&route("show"));
            block.push(';');
        }
        block.push_str(&format!("\nr.resource(\"{}\")", m.url));
        for action in rest {
            block.push_str(&route(action));
        }
        if auth {
            block.push_str("\n    .middleware(\"auth\");");
        } else {
            block.push(';');
        }
        return block;
    }
    if auth {
        block.push_str(&format!(
            "// {}: viewing is public; creating, editing and deleting need a signed-in user.\n",
            m.url
        ));
        block.push_str(&format!("r.resource(\"{}\")", m.url));
        for action in ["index", "show"] {
            block.push_str(&route(action));
        }
        block.push_str(&format!(";\nr.resource(\"{}\")", m.url));
        for action in ["create", "store", "edit", "update", "destroy"] {
            block.push_str(&route(action));
        }
        block.push_str("\n    .middleware(\"auth\");");
    } else {
        block.push_str(&format!(
            "// {}: public, anyone can create, edit and delete records.\n",
            m.url
        ));
        block.push_str(&format!("r.resource(\"{}\")", m.url));
        for action in [
            "index", "create", "store", "show", "edit", "update", "destroy",
        ] {
            block.push_str(&route(action));
        }
        block.push(';');
    }
    block
}

/// Whether the app has authentication: `bootstrap/app.rs` registers the user model with `.temper(…)` (Temper) or
/// `.auth::<User>()`, which also register the `auth` middleware the resource routes use.
fn has_auth(ctx: Ctx<'_>) -> bool {
    std::fs::read_to_string(ctx.root.join("bootstrap").join("app.rs")).is_ok_and(|text| {
        text.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with(".auth::<") || l.starts_with(".temper(")
        })
    })
}

/// `tests/<table>_search.rs`: the generated list's search, rendered without and with a search text.
fn search_test_plan(ctx: Ctx<'_>, name: &Name, m: &ModelCtx, hl: &str) -> anyhow::Result<Plan> {
    #[derive(Serialize)]
    struct C<'a> {
        m: &'a ModelCtx,
        k: Option<&'a KitCtx>,
        lib: String,
        hl: &'a str,
    }
    let frontend = app_frontend(ctx)?;
    let kit = frontend.is_js().then(|| KitCtx::new(frontend, name, m));
    let lib = crate::new::lib_name(&crate::cmd::package_name(ctx.root)?);
    let mut plan = Plan::default();
    plan.file(
        format!("tests/{}_search.rs", m.table),
        render(
            SEARCH_TEST,
            C {
                m,
                k: kit.as_ref(),
                lib,
                hl,
            },
        )?,
    );
    Ok(plan)
}

/// Whether the app has the Prospect building block: `bootstrap/app.rs` calls `.prospect(…)`.
fn has_search(ctx: Ctx<'_>) -> bool {
    std::fs::read_to_string(ctx.root.join("bootstrap").join("app.rs")).is_ok_and(|text| {
        text.lines()
            .any(|l| l.trim_start().starts_with(".prospect("))
    })
}

/// `make:migration`.
pub(crate) fn migration(ctx: Ctx<'_>, args: &MigrationArgs) -> anyhow::Result<Plan> {
    let snake = Name::parse(&args.name)?.snake();
    let dir = ctx.root.join("database/migrations");
    let suffix = format!("_{snake}.rs");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file.starts_with('m') && file.ends_with(&suffix) {
                bail!("database/migrations/{file} already exists; pick another migration name");
            }
        }
    }
    migration_plan(ctx, &snake, &[], None)
}

/// The stamp for a new migration: the clock's, or one second after the newest migration in
/// `database/migrations/` when that is not older. Migrations made in the same second (`make:model -m` followed
/// by `make:migration`, or a script) then still sort in the order they were made.
pub(super) fn unique_stamp(ctx: Ctx<'_>) -> String {
    let newest = std::fs::read_dir(ctx.root.join("database").join("migrations"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let s = name.strip_prefix('m')?.get(..17)?.to_owned();
            let ok = s.bytes().enumerate().all(|(i, b)| {
                if matches!(i, 4 | 7 | 10) {
                    b == b'_'
                } else {
                    b.is_ascii_digit()
                }
            });
            ok.then_some(s)
        })
        .max();
    let mut secs = ctx.now;
    if let Some(newest) = newest {
        // Stamps compare as text in time order; a far-future stamp (a wrong clock) is not chased forever.
        for _ in 0..1_000_000 {
            if stamp(secs) > newest {
                break;
            }
            secs += 1;
        }
    }
    stamp(secs)
}

#[derive(Serialize)]
struct MigrationCtx {
    doc: String,
    struct_doc: String,
    struct_name: String,
    name: String,
    kind: &'static str,
    table: String,
    column: String,
    columns: Vec<String>,
    /// The search index's columns and weights (a searchable model's create-table or search-index migration).
    search: Vec<SearchColumn>,
}

/// The migration that adds a searchable model's index to its existing table (`--searchable` without `-m`).
fn search_migration_plan(
    ctx: Ctx<'_>,
    snake: &str,
    table: &str,
    search: &SearchCtx,
) -> anyhow::Result<Plan> {
    let stamp = unique_stamp(ctx);
    let module = format!("m{stamp}_{snake}");
    let c = MigrationCtx {
        doc: format!("Add the search index of the `{table}` table (Prospect)."),
        struct_doc: format!("Creates the search index of `{table}`."),
        struct_name: Name::parse(snake)?.pascal(),
        name: format!("{stamp}_{snake}"),
        kind: "search",
        table: table.to_owned(),
        column: String::new(),
        columns: Vec::new(),
        search: search.texts.clone(),
    };
    migration_files(&module, &c)
}

fn migration_files(module: &str, c: &MigrationCtx) -> anyhow::Result<Plan> {
    let mut plan = Plan::default();
    plan.file(
        format!("database/migrations/{module}.rs"),
        render(MIGRATION, c)?,
    );
    plan.insert(
        "database/migrations/mod.rs",
        MODS,
        format!("pub mod {module};"),
    );
    plan.insert(
        "database/migrations/mod.rs",
        MIGRATIONS,
        format!("m.add({module}::{});", c.struct_name),
    );
    Ok(plan)
}

fn migration_plan(
    ctx: Ctx<'_>,
    snake: &str,
    fields: &[Field],
    search: Option<&SearchCtx>,
) -> anyhow::Result<Plan> {
    type C = MigrationCtx;
    let stamp = unique_stamp(ctx);
    let module = format!("m{stamp}_{snake}");
    let struct_name = Name::parse(snake)?.pascal();
    let mut c = C {
        doc: format!("The `{snake}` migration."),
        struct_doc: "Changes the schema.".to_owned(),
        struct_name,
        name: format!("{stamp}_{snake}"),
        kind: "blank",
        table: String::new(),
        column: String::new(),
        columns: fields.iter().map(Field::blueprint).collect(),
        search: Vec::new(),
    };
    let body = snake.strip_suffix("_table");
    if let Some(table) = body
        .and_then(|b| b.strip_prefix("create_"))
        .filter(|t| !t.is_empty())
    {
        c.kind = "create";
        c.doc = format!("Create the `{table}` table.");
        c.struct_doc = format!("Creates `{table}`.");
        c.table = table.to_owned();
        if let Some(search) = search {
            c.search = search.texts.clone();
        }
    } else if let Some((column, table)) = body
        .and_then(|b| b.strip_prefix("add_"))
        .and_then(|b| b.split_once("_to_"))
        .filter(|(col, table)| !col.is_empty() && !table.is_empty())
    {
        c.kind = "table";
        c.doc = format!("Add columns to the `{table}` table.");
        c.struct_doc = format!("Adds columns to `{table}`.");
        c.table = table.to_owned();
        c.column = column.to_owned();
    }
    migration_files(&module, &c)
}

/// `make:factory`.
pub(crate) fn factory(ctx: Ctx<'_>, args: &FactoryArgs) -> anyhow::Result<Plan> {
    let name = Name::parse(&args.name)?.without_suffix("factory");
    let model = match &args.model {
        Some(m) => Name::parse(m)?,
        None => name,
    };
    let fields = existing_model(ctx, &model)?;
    factory_plan(&ModelCtx::new(&model, &fields))
}

fn factory_plan(m: &ModelCtx) -> anyhow::Result<Plan> {
    #[derive(Serialize)]
    struct C<'a> {
        m: &'a ModelCtx,
        struct_name: String,
    }
    let module = format!("{}_factory", m.snake);
    let c = C {
        m,
        struct_name: format!("{}Factory", m.pascal),
    };
    let mut plan = Plan::default();
    plan.file(
        format!("database/factories/{module}.rs"),
        render(FACTORY, &c)?,
    );
    plan.insert(
        "database/factories/mod.rs",
        MODS,
        format!("pub mod {module};"),
    );
    Ok(plan)
}

/// `make:seeder`.
pub(crate) fn seeder(ctx: Ctx<'_>, args: &SeederArgs) -> anyhow::Result<Plan> {
    let name = Name::parse(&args.name)?.without_suffix("seeder");
    let factory_file = ctx
        .root
        .join(format!("database/factories/{}_factory.rs", name.snake()));
    let m = ModelCtx::new(&name, &[]);
    seeder_plan(&name, factory_file.is_file().then_some(&m))
}

fn seeder_plan(name: &Name, factory: Option<&ModelCtx>) -> anyhow::Result<Plan> {
    #[derive(Serialize)]
    struct C {
        struct_name: String,
        model: String,
        factory: Option<String>,
        factory_module: String,
    }
    let module = format!("{}_seeder", name.snake());
    let c = C {
        struct_name: format!("{}Seeder", name.pascal()),
        model: name.pascal(),
        factory: factory.map(|m| format!("{}Factory", m.pascal)),
        factory_module: format!("{}_factory", name.snake()),
    };
    let mut plan = Plan::default();
    plan.file(format!("database/seeders/{module}.rs"), render(SEEDER, &c)?);
    plan.insert(
        "database/seeders/mod.rs",
        MODS,
        format!("pub mod {module};"),
    );
    plan.insert(
        "database/seeders/mod.rs",
        SEEDERS,
        format!("s.add({module}::{});", c.struct_name),
    );
    Ok(plan)
}

/// `make:command`.
pub(crate) fn command(_ctx: Ctx<'_>, args: &CommandArgs) -> anyhow::Result<Plan> {
    #[derive(Serialize)]
    struct C {
        struct_name: String,
        command: String,
        about: String,
    }
    let name = Name::parse(&args.name)?.without_suffix("command");
    let module = name.snake();
    let c = C {
        struct_name: name.pascal(),
        command: name.kebab(),
        about: name.sentence(),
    };
    let mut plan = Plan::default();
    plan.file(format!("app/commands/{module}.rs"), render(COMMAND, &c)?);
    plan.insert("app/commands/mod.rs", MODS, format!("pub mod {module};"));
    plan.insert(
        "app/commands/mod.rs",
        COMMANDS,
        format!("c.add({module}::{});", c.struct_name),
    );
    plan.notes
        .push(format!("run it with: smeltery {}", c.command));
    Ok(plan)
}

/// `make:middleware`.
pub(crate) fn middleware(_ctx: Ctx<'_>, args: &MiddlewareArgs) -> anyhow::Result<Plan> {
    #[derive(Serialize)]
    struct C {
        fn_name: String,
    }
    let fn_name = Name::parse(&args.name)?
        .without_suffix("middleware")
        .snake();
    let mut plan = Plan::default();
    plan.file(
        format!("app/middleware/{fn_name}.rs"),
        render(
            MIDDLEWARE,
            C {
                fn_name: fn_name.clone(),
            },
        )?,
    );
    plan.insert("app/middleware/mod.rs", MODS, format!("pub mod {fn_name};"));
    plan.notes.push(format!(
        "register it in bootstrap/app.rs:\n    .middleware(\"{fn_name}\", app::middleware::{fn_name}::{fn_name})\n\
         then attach it to routes or groups with .middleware(\"{fn_name}\")"
    ));
    Ok(plan)
}

/// Why an app without `app/agents/mod.rs` gets no agents or jobs (D-506).
const NO_WATCHFIRE: &str = "this app has no Watchfire (no app/agents/mod.rs: it was created with `--smelt` without \
                            `watchfire`), so it takes no agents or jobs";

/// Refuses before anything is written when the app has no Watchfire registration file.
fn require_watchfire(ctx: Ctx<'_>) -> anyhow::Result<()> {
    if !ctx.root.join("app").join("agents").join("mod.rs").is_file() {
        bail!("{NO_WATCHFIRE}; nothing was changed");
    }
    Ok(())
}

/// `make:agent`.
pub(crate) fn agent(ctx: Ctx<'_>, args: &AgentArgs) -> anyhow::Result<Plan> {
    require_watchfire(ctx)?;
    #[derive(Serialize)]
    struct C {
        struct_name: String,
        agent: String,
    }
    let name = Name::parse(&args.name)?.without_suffix("agent");
    let module = name.snake();
    let c = C {
        struct_name: name.pascal(),
        agent: module.clone(),
    };
    let mut plan = Plan::default();
    plan.file(format!("app/agents/{module}.rs"), render(AGENT, &c)?);
    plan.insert("app/agents/mod.rs", MODS, format!("pub mod {module};"));
    plan.insert(
        "app/agents/mod.rs",
        AGENTS,
        format!("w.agent({module}::{}::default());", c.struct_name),
    );
    Ok(plan)
}

/// `make:job`.
pub(crate) fn job(ctx: Ctx<'_>, args: &JobArgs) -> anyhow::Result<Plan> {
    require_watchfire(ctx)?;
    #[derive(Serialize)]
    struct C {
        struct_name: String,
        job: String,
    }
    let name = Name::parse(&args.name)?.without_suffix("job");
    let module = name.snake();
    let c = C {
        struct_name: name.pascal(),
        job: module.clone(),
    };
    let mut plan = Plan::default();
    plan.file(format!("app/jobs/{module}.rs"), render(JOB, &c)?);
    plan.insert("app/jobs/mod.rs", MODS, format!("pub mod {module};"));
    plan.insert(
        "app/agents/mod.rs",
        AGENTS,
        format!("w.job::<crate::app::jobs::{module}::{}>();", c.struct_name),
    );
    plan.notes.push(format!(
        "dispatch it with: crate::app::jobs::{module}::{} {{}}.dispatch(&app).await?",
        c.struct_name
    ));
    Ok(plan)
}

/// `make:page` (React / Vue apps): one page, its controller and its route.
pub(crate) fn page(ctx: Ctx<'_>, args: &PageArgs) -> anyhow::Result<Plan> {
    if !has_web(ctx) {
        bail!("{NO_WEB}; nothing was changed");
    }
    let frontend = app_frontend(ctx)?;
    if !frontend.is_js() {
        bail!(
            "`make:page` writes React and Vue pages, and this app uses Mold; `smeltery make:controller Name` writes a \
             controller with a Mold view and its route. Nothing was changed"
        );
    }
    let name = Name::parse(&args.name)?.without_suffix("page");
    let react = frontend == Frontend::React;
    let component = if react { name.kebab() } else { name.pascal() };
    let c = PageCtx {
        module: name.snake(),
        views: name.snake(),
        title: name.sentence(),
        alloy: true,
        react,
        file: format!(
            "resources/js/pages/{component}.{}",
            if react { "tsx" } else { "vue" }
        ),
        component,
        function: name.pascal(),
        handler: "show".to_owned(),
        url: format!("/{}", name.kebab()),
    };
    let mut plan = Plan::default();
    plan.file(
        format!("app/controllers/{}.rs", c.module),
        render(CONTROLLER, &c)?,
    );
    plan.file(
        &c.file,
        render(if react { REACT_PAGE } else { VUE_PAGE }, &c)?,
    );
    plan.insert(
        "app/controllers/mod.rs",
        MODS,
        format!("pub mod {};", c.module),
    );
    plan.insert(
        "routes/web.rs",
        ROUTES,
        get_route(&c.url, &c.module, "show", &name.kebab()),
    );
    Ok(plan)
}

/// The `routes/web.rs` entry of a page: `r.get("/url", crate::app::controllers::module::handler).name("name");`,
/// laid out as rustfmt lays it out (the arguments go on their own lines past its 60-column call width).
fn get_route(url: &str, module: &str, handler: &str, route: &str) -> String {
    let target = format!("crate::app::controllers::{module}::{handler}");
    if url.len() + 4 + target.len() <= 60 {
        format!("r.get(\"{url}\", {target})\n    .name(\"{route}\");")
    } else {
        format!("r.get(\n    \"{url}\",\n    {target},\n)\n.name(\"{route}\");")
    }
}

/// `make:spark`.
pub(crate) fn spark(ctx: Ctx<'_>, args: &SparkArgs) -> anyhow::Result<Plan> {
    let frontend = if ctx.root.join("Cargo.toml").is_file() {
        app_frontend(ctx)?
    } else {
        Frontend::Mold
    };
    if frontend.is_js() {
        let kit = if frontend == Frontend::React {
            "React"
        } else {
            "Vue"
        };
        bail!(
            "this app uses the {kit} starter kit, whose pages are {kit} components; Sparks are live components for \
             Mold views. Write a page with `smeltery make:page Name` instead. Nothing was changed"
        );
    }
    #[derive(Serialize)]
    struct C {
        struct_name: String,
        spark: String,
        message_echo: String,
        saved_echo: String,
    }
    let name = Name::parse(&args.name)?.without_suffix("spark");
    let module = name.snake();
    let c = C {
        struct_name: name.pascal(),
        spark: module.clone(),
        message_echo: echo("message"),
        saved_echo: echo("saved"),
    };
    let mut plan = Plan::default();
    if let (Some(channel), Some(event)) = (&args.listen, &args.event) {
        let l = listener_ctx(ctx, &name, channel, event)?;
        plan.file(
            format!("app/sparks/{module}.rs"),
            render(SPARK_LISTENER, &l)?,
        );
        plan.file(
            format!("resources/views/sparks/{module}.mold.html"),
            render(SPARK_LISTENER_VIEW, &l)?,
        );
    } else {
        plan.file(format!("app/sparks/{module}.rs"), render(SPARK, &c)?);
        plan.file(
            format!("resources/views/sparks/{module}.mold.html"),
            render(SPARK_VIEW, &c)?,
        );
    }
    plan.insert("app/sparks/mod.rs", MODS, format!("pub mod {module};"));
    plan.insert(
        "app/sparks/mod.rs",
        SPARKS,
        format!("s.add::<{module}::{}>();", c.struct_name),
    );
    plan.notes
        .push(format!("show it on a page with: @spark(\"{module}\")"));
    if args.listen.is_some() {
        plan.notes.push(
            "the channel rules of routes/channels.rs decide who receives its events (a public channel must be \
             declared there with `c.public(…)`)"
                .to_owned(),
        );
    }
    Ok(plan)
}

/// What a listening Spark (`make:spark Name --listen <channel> --event <Event>`) needs.
#[derive(Serialize)]
struct ListenerCtx {
    struct_name: String,
    spark: String,
    channel: String,
    channel_echo: String,
    /// The name the event is sent with (`App\Events\OrderShipped`, `order.shipped`).
    event_name: String,
    /// The same as a Rust string literal's contents (backslashes doubled).
    event_rust: String,
    /// The struct its data deserializes into.
    data_struct: String,
    fields: Vec<ListenerField>,
    received_echo: String,
}

#[derive(Serialize)]
struct ListenerField {
    name: String,
    ty: &'static str,
}

/// Checks `--listen` / `--event` (an app with Anvil, a channel name Anvil accepts) and names the parts.
fn listener_ctx(
    ctx: Ctx<'_>,
    name: &Name,
    channel: &str,
    event: &str,
) -> anyhow::Result<ListenerCtx> {
    let bootstrap =
        std::fs::read_to_string(ctx.root.join("bootstrap").join("app.rs")).unwrap_or_default();
    if !bootstrap
        .lines()
        .any(|l| l.trim_start().starts_with(".anvil("))
    {
        bail!(
            "this app has no Anvil building block (no `.anvil(` in bootstrap/app.rs): a listening Spark needs \
             broadcasting; nothing was changed"
        );
    }
    // `{field}` parts become fields; the rest must be a channel name's characters.
    let mut fields = Vec::new();
    let mut plain = String::new();
    let mut rest = channel;
    while let Some(open) = rest.find('{') {
        plain.push_str(&rest[..open]);
        let close = rest[open..]
            .find('}')
            .map(|c| open + c)
            .with_context(|| format!("--listen {channel}: a `{{` without `}}`"))?;
        let field = &rest[open + 1..close];
        let ok = field.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && field
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !ok || field == "received" {
            bail!(
                "--listen {channel}: `{{{field}}}` is not a field name (lowercase letters, digits and `_`)"
            );
        }
        if !fields.iter().any(|f: &ListenerField| f.name == field) {
            fields.push(ListenerField {
                name: field.to_owned(),
                ty: if field.ends_with("_id") || field == "id" {
                    "i64"
                } else {
                    "String"
                },
            });
        }
        plain.push('x');
        rest = &rest[close + 1..];
    }
    plain.push_str(rest);
    let allowed = |c: char| c.is_ascii_alphanumeric() || "_-=@,.;".contains(c);
    if plain.is_empty() || plain.len() > 164 || !plain.chars().all(allowed) {
        bail!(
            "--listen {channel}: a channel name is letters, digits and `_ - = @ , . ;` (with `private-` or \
             `presence-` in front for those channels)"
        );
    }
    // An event type (`OrderShipped`) is sent as `App\Events\OrderShipped`; anything else is the name as sent.
    let is_type = event.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && event.chars().all(|c| c.is_ascii_alphanumeric());
    let event_name = if is_type {
        format!("App\\Events\\{event}")
    } else {
        event.to_owned()
    };
    if event_name.is_empty() || event_name.chars().any(|c| c.is_control() || c == '"') {
        bail!("--event {event}: not an event name");
    }
    Ok(ListenerCtx {
        struct_name: name.pascal(),
        spark: name.snake(),
        channel: channel.to_owned(),
        channel_echo: channel.to_owned(),
        event_rust: event_name.replace('\\', "\\\\"),
        event_name,
        data_struct: if is_type {
            event.to_owned()
        } else {
            format!("{}Event", name.pascal())
        },
        fields,
        received_echo: echo("received"),
    })
}

/// `make:mail`.
pub(crate) fn mail(ctx: Ctx<'_>, args: &MailArgs) -> anyhow::Result<Plan> {
    #[derive(Serialize)]
    struct C {
        struct_name: String,
        view: String,
        subject: String,
        lower: String,
        web: bool,
        name_echo: String,
        email_echo: String,
        home_echo: String,
    }
    let name = Name::parse(&args.name)?.without_suffix("mail");
    let module = name.snake();
    let c = C {
        struct_name: name.pascal(),
        view: module.clone(),
        subject: name.sentence(),
        lower: name.words.join(" "),
        web: ctx.root.join("routes").is_dir(),
        name_echo: echo("name"),
        email_echo: echo("email"),
        home_echo: echo("route(\"home\")"),
    };
    let mut plan = Plan::default();
    plan.file(format!("app/mail/{module}.rs"), render(MAIL, &c)?);
    plan.file(
        format!("resources/views/mail/{}.mold.html", c.view),
        render(MAIL_VIEW, &c)?,
    );
    plan.insert("app/mail/mod.rs", MODS, format!("pub mod {module};"));
    plan.notes.push(format!(
        "send it from a handler with a `mailer: Mailer` argument:\n    \
         mailer.send(crate::app::mail::{module}::{} {{ name, email }}).await?;\n\
         (`mailer.queue(…)` sends it from the Watchfire queue)",
        c.struct_name
    ));
    Ok(plan)
}
