# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- The database driver highlights a value that holds U+E000 / U+E001 in Rust, so a stored marker pair never shows
  as a match. The SQLite index's update trigger also fires when a row's key changes.
- `Searchable` (`index(&mut IndexSpec)`, `search(&prospect, text)`), `IndexSpec` (`text(..).weight(..)`, `filter`,
  `sort`, `only_when`, `scoped_by`, `name`, `language`), `Weight` (`A`-`D`) and `Language` (`Simple`, `English`).
  Specs are checked against the entity when the app builds (columns, types, one integer primary key).
- `ProspectExt::prospect(|p| { p.model::<M>(); })` / `prospect_with(settings, …)`, the `Prospect` service and
  extractor, `ProspectSettings` (`PROSPECT_DRIVER`, `PROSPECT_MAX_QUERY_LENGTH`, `PROSPECT_MAX_PER_PAGE`,
  `PROSPECT_BATCH`), `Driver::{Database, Memory}`.
- `SearchText::parse`: terms of letters and digits (at most 16, of at most 200 characters of text), all required,
  the last one a prefix from two characters.
- The `Search` builder: `within` / `across_scopes`, `where_eq` / `where_in` / `where_not_in` / `where_between` on
  declared filter columns, `order_by` / `order_by_relevance`, `highlight` (at most 4 text columns), `query` (a SeaORM
  refinement, database driver only), `paginate` (`Page<Hit<M>>`), `get`, `keys`, `count`. `Hit` (model, score,
  highlights), `Highlights`, `Highlight`, `Segment`. `ProspectError`.
- The `database` driver: SQLite FTS5 (external content, triggers, `bm25` with column weights, `highlight` /
  `snippet`), PostgreSQL (generated `tsvector`, GIN, `ts_rank_cd`, `ts_headline` over the page's rows), MySQL /
  MariaDB (`FULLTEXT`, boolean mode without the words InnoDB does not index: `MYSQL_STOPWORDS` and words under
  `innodb_ft_min_token_size`; highlights in Rust). The index is checked on first use; a missing one is an
  error naming the migration to write.
- `migration::SearchIndex` (`on`, `key`, `text`, `language`, `create`, `drop`, `rebuild`, `create_sql`, `drop_sql`).
- The `memory` engine (documents written by model events, hits re-checked in SQL), `Prospect::import`, `sync`,
  `flush`, `paused`.
- Console commands `prospect:status`, `prospect:import`, `prospect:flush`.
- `testing::fake(&app)` → `FakeSearch` (`assert_indexed`, `assert_not_indexed`, `assert_synced_times`, `is_indexed`,
  `indexed_text`, `indexed_columns`, `count`).

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
