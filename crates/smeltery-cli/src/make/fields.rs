//! Model fields: `title:string`, `body:text?`, `user_id:foreign`.

use anyhow::bail;

use super::names::{is_keyword, plural};

/// A field type of `make:model`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldType {
    String,
    Text,
    Integer,
    Bigint,
    Bool,
    Float,
    Date,
    Datetime,
    Json,
    Uuid,
    Foreign,
    /// An uploaded file; the column holds its stored path.
    File,
}

const TYPES: &[(&str, FieldType)] = &[
    ("string", FieldType::String),
    ("text", FieldType::Text),
    ("integer", FieldType::Integer),
    ("bigint", FieldType::Bigint),
    ("bool", FieldType::Bool),
    ("float", FieldType::Float),
    ("date", FieldType::Date),
    ("datetime", FieldType::Datetime),
    ("json", FieldType::Json),
    ("uuid", FieldType::Uuid),
    ("foreign", FieldType::Foreign),
    ("file", FieldType::File),
];

impl FieldType {
    fn parse(s: &str) -> Option<Self> {
        TYPES.iter().find(|(name, _)| *name == s).map(|(_, t)| *t)
    }

    /// The Rust type of the model field.
    pub(crate) fn rust(self) -> &'static str {
        match self {
            FieldType::String | FieldType::Text | FieldType::File => "String",
            FieldType::Integer => "i32",
            FieldType::Bigint | FieldType::Foreign => "i64",
            FieldType::Bool => "bool",
            FieldType::Float => "f64",
            FieldType::Date => "Date",
            FieldType::Datetime => "DateTimeUtc",
            FieldType::Json => "Json",
            FieldType::Uuid => "Uuid",
        }
    }

    /// True for types an HTML form and a Mold view handle directly (text, numbers, checkboxes).
    pub(crate) fn in_forms(self) -> bool {
        matches!(
            self,
            FieldType::String
                | FieldType::Text
                | FieldType::Integer
                | FieldType::Bigint
                | FieldType::Bool
                | FieldType::Float
                | FieldType::Foreign
                | FieldType::File
        )
    }
}

/// The doc line `make:model` writes above a `file` field, so later generators read the type back.
pub(crate) const FILE_DOC: &str =
    "/// A stored upload: its path under `storage/app/public` (a `file` field).";

/// One model field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field {
    pub(crate) name: String,
    pub(crate) ty: FieldType,
    pub(crate) nullable: bool,
}

fn valid_types() -> String {
    TYPES.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
}

impl Field {
    /// Parses `name:type` with an optional trailing `?` (nullable).
    pub(crate) fn parse(spec: &str) -> anyhow::Result<Self> {
        let Some((name, ty)) = spec.split_once(':') else {
            bail!("invalid field `{spec}`: write `name:type`, e.g. `title:string` or `body:text?`");
        };
        let (ty, nullable) = match ty.strip_suffix('?') {
            Some(t) => (t, true),
            None => (ty, false),
        };
        let Some(ty) = FieldType::parse(ty) else {
            bail!(
                "unknown field type `{ty}` in `{spec}`; valid types: {}",
                valid_types()
            );
        };
        let valid_name = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !valid_name {
            bail!("invalid field name `{name}` in `{spec}`: use snake_case (`published_at`)");
        }
        if is_keyword(name) {
            bail!("invalid field name `{name}` in `{spec}`: it is a Rust keyword");
        }
        if ["id", "created_at", "updated_at"].contains(&name) {
            bail!("`{name}` is added to every model; leave it out");
        }
        if ty == FieldType::Foreign && !name.ends_with("_id") {
            bail!("a foreign field names the key: `{name}_id:foreign`, not `{name}:foreign`");
        }
        Ok(Field {
            name: name.to_owned(),
            ty,
            nullable,
        })
    }

    /// Parses every spec, rejecting duplicate names.
    pub(crate) fn parse_all(specs: &[String]) -> anyhow::Result<Vec<Self>> {
        let mut fields: Vec<Field> = Vec::new();
        for spec in specs {
            let field = Field::parse(spec)?;
            if fields.iter().any(|f| f.name == field.name) {
                bail!("field `{}` is given twice", field.name);
            }
            fields.push(field);
        }
        Ok(fields)
    }

    /// The Rust type, wrapped in `Option` when nullable.
    pub(crate) fn rust_type(&self) -> String {
        if self.nullable {
            format!("Option<{}>", self.ty.rust())
        } else {
            self.ty.rust().to_owned()
        }
    }

    /// The table a foreign key points at (`user_id` → `users`).
    pub(crate) fn foreign_table(&self) -> String {
        plural(self.name.strip_suffix("_id").unwrap_or(&self.name))
    }

    /// The migration line, e.g. `t.text("body").nullable();`.
    pub(crate) fn blueprint(&self) -> String {
        let call = match self.ty {
            FieldType::String | FieldType::File => "string",
            FieldType::Text => "text",
            FieldType::Integer => "integer",
            FieldType::Bigint => "big_integer",
            FieldType::Bool => "boolean",
            FieldType::Float => "double",
            FieldType::Date => "date",
            FieldType::Datetime => "datetime",
            FieldType::Json => "json",
            FieldType::Uuid => "uuid",
            FieldType::Foreign => "foreign_id",
        };
        let mut line = format!("t.{call}(\"{}\")", self.name);
        if self.ty == FieldType::Foreign {
            line.push_str(&format!(".constrained(\"{}\")", self.foreign_table()));
        }
        if self.nullable {
            line.push_str(".nullable()");
        }
        line.push(';');
        line
    }

    /// The factory value for this field, e.g. `fake.sentence(4)`.
    pub(crate) fn fake(&self) -> String {
        let value = match self.ty {
            FieldType::String => match self.name.as_str() {
                "name" => "fake.name()".to_owned(),
                "first_name" => "fake.first_name()".to_owned(),
                "last_name" => "fake.last_name()".to_owned(),
                "email" => "fake.unique_email()".to_owned(),
                "title" | "subject" => "fake.sentence(4)".to_owned(),
                _ => "fake.words(3)".to_owned(),
            },
            FieldType::Text => "fake.paragraph()".to_owned(),
            FieldType::Integer => "fake.int(1..=100) as i32".to_owned(),
            FieldType::Bigint => "fake.int(1..=1000)".to_owned(),
            FieldType::Bool => "fake.bool()".to_owned(),
            FieldType::Float => "fake.int(0..=10000) as f64 / 100.0".to_owned(),
            FieldType::Date => "fake.date()".to_owned(),
            FieldType::Datetime => "DateTimeUtc::default()".to_owned(),
            FieldType::Json => "smeltery::json!({})".to_owned(),
            FieldType::Uuid => "Uuid::parse_str(&fake.uuid()).unwrap_or_default()".to_owned(),
            FieldType::Foreign => "1".to_owned(),
            FieldType::File => "\"files/example.txt\".to_owned()".to_owned(),
        };
        if self.nullable {
            format!("Some({value})")
        } else {
            value
        }
    }

    /// Reads the fields of `app/models/<name>.rs` (written by `make:model`), skipping `id` and the timestamps.
    pub(crate) fn from_model_source(source: &str) -> Vec<Field> {
        let mut fields = Vec::new();
        let mut inside = false;
        let mut file_doc = false;
        for line in source.lines() {
            let line = line.trim();
            let after_file_doc = std::mem::replace(&mut file_doc, line == FILE_DOC);
            if line.starts_with("pub struct Model") {
                inside = true;
                continue;
            }
            if !inside {
                continue;
            }
            if line.starts_with('}') {
                break;
            }
            let Some((name, ty)) = line
                .strip_prefix("pub ")
                .and_then(|l| l.strip_suffix(','))
                .and_then(|l| l.split_once(':'))
            else {
                continue;
            };
            let (name, ty) = (name.trim(), ty.trim());
            if ["id", "created_at", "updated_at"].contains(&name) {
                continue;
            }
            let (ty, nullable) = match ty.strip_prefix("Option<").and_then(|t| t.strip_suffix('>'))
            {
                Some(inner) => (inner, true),
                None => (ty, false),
            };
            let ty = match ty {
                "String" if after_file_doc => FieldType::File,
                "String" => FieldType::String,
                "i32" => FieldType::Integer,
                "i64" if name.ends_with("_id") => FieldType::Foreign,
                "i64" => FieldType::Bigint,
                "bool" => FieldType::Bool,
                "f64" => FieldType::Float,
                "Date" => FieldType::Date,
                "DateTimeUtc" => FieldType::Datetime,
                "Json" => FieldType::Json,
                "Uuid" => FieldType::Uuid,
                _ => continue,
            };
            fields.push(Field {
                name: name.to_owned(),
                ty,
                nullable,
            });
        }
        fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_types_and_nullability() {
        let f = Field::parse("body:text?").unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(
            f,
            Field {
                name: "body".into(),
                ty: FieldType::Text,
                nullable: true
            }
        );
        assert_eq!(f.rust_type(), "Option<String>");
        assert_eq!(f.blueprint(), "t.text(\"body\").nullable();");
        let f = Field::parse("user_id:foreign").unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(
            f.blueprint(),
            "t.foreign_id(\"user_id\").constrained(\"users\");"
        );
        let f = Field::parse("category_id:foreign?").unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(
            f.blueprint(),
            "t.foreign_id(\"category_id\").constrained(\"categories\").nullable();"
        );
        let f = Field::parse("image:file?").unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(
            (f.rust_type().as_str(), f.blueprint().as_str()),
            ("Option<String>", "t.string(\"image\").nullable();")
        );
        assert_eq!(
            Field::parse("views:bigint")
                .map(|f| f.blueprint())
                .ok()
                .as_deref(),
            Some("t.big_integer(\"views\");")
        );
    }

    #[test]
    fn bad_fields_say_what_is_valid() {
        let msg = Field::parse("title:strng")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert_eq!(
            msg,
            "unknown field type `strng` in `title:strng`; valid types: string, text, integer, bigint, bool, \
             float, date, datetime, json, uuid, foreign, file"
        );
        for bad in [
            "title",
            "Title:string",
            "type:string",
            "id:bigint",
            "user:foreign",
            "a b:string",
            ":string",
        ] {
            assert!(Field::parse(bad).is_err(), "{bad}");
        }
        let twice = Field::parse_all(&["a:string".into(), "a:text".into()]);
        assert!(twice.is_err());
    }

    #[test]
    fn reads_fields_back_from_a_model() {
        let src = "pub struct Model {\n    #[sea_orm(primary_key)]\n    pub id: i64,\n    pub title: String,\n    \
                   pub body: Option<String>,\n    pub user_id: i64,\n    pub score: f64,\n    \
                   /// A stored upload: its path under `storage/app/public` (a `file` field).\n    \
                   pub image: Option<String>,\n    \
                   pub created_at: Option<DateTimeUtc>,\n}\n";
        let fields = Field::from_model_source(src);
        let names: Vec<_> = fields
            .iter()
            .map(|f| (f.name.as_str(), f.ty, f.nullable))
            .collect();
        assert_eq!(
            names,
            [
                ("title", FieldType::String, false),
                ("body", FieldType::String, true),
                ("user_id", FieldType::Foreign, false),
                ("score", FieldType::Float, false),
                ("image", FieldType::File, true),
            ]
        );
    }
}
