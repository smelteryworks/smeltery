# Skill: write a migration

1. Generate it:
   - a new table: `smeltery make:migration create_invoices_table` (or `smeltery make:model Invoice … -m`);
   - new columns: `smeltery make:migration add_due_at_to_invoices_table`;
   - anything else: `smeltery make:migration backfill_invoice_numbers`.

   The file is `database/migrations/m<timestamp>_<name>.rs`, registered in `database/migrations/mod.rs`.
2. Write `up` with the schema builder: `schema.create("invoices", |t| { t.id(); t.string("number").unique();
   t.foreign_id("user_id").constrained("users"); t.datetime("due_at").nullable(); t.timestamps(); }).await`, or
   `schema.table("invoices", |t| { … })` to add columns. Column types: `string`, `string_len`, `text`, `integer`,
   `big_integer`, `boolean`, `float`, `double`, `decimal`, `date`, `datetime`, `json`, `uuid`, `foreign_id`; modifiers
   `.nullable()`, `.unique()`, `.default(v)`, `.index()`.
3. Write `down` so it undoes `up` (`schema.drop_if_exists("invoices").await` for a new table).
4. If a model belongs to the table, add or update its field in `app/models/<name>.rs`.
5. Run `smeltery migrate`, check `smeltery migrate:status`; `smeltery migrate:rollback` undoes the last batch.
6. Never edit a migration that has already run anywhere; add a new one.
