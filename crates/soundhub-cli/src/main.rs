//! Headless soundbuch CLI for local smoke-testing without the desktop shell.
//!
//! Usage: `soundhub-cli <command> ...`
//! Run `soundhub-cli help` for the command list.

use std::path::{Path, PathBuf};

use soundhub_core::collections::{self, evaluate};
use soundhub_core::db::Repo;
use soundhub_core::import::{ImportOutcome, ImportPipeline};
use soundhub_core::library::Library;
use soundhub_core::models::*;
use soundhub_core::search::{search, SearchQuery};
use soundhub_core::{AssetId, RowId};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = &args[1..];

    let result = match cmd {
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        "init" => cmd_init(rest),
        "import" => cmd_import(rest),
        "list" => cmd_list(rest),
        "search" => cmd_search(rest),
        "tag" => cmd_tag(rest),
        "untag" => cmd_untag(rest),
        "person" => cmd_person(rest),
        "batch-tag" => cmd_batch_tag(rest),
        "smart" => cmd_smart(rest),
        "members" => cmd_members(rest),
        "gps" => cmd_gps(rest),
        "info" => cmd_info(rest),
        "trash" => cmd_trash(rest),
        "delete" => cmd_delete(rest),
        "restore" => cmd_restore(rest),
        "purge" => cmd_purge(rest),
        "empty-trash" => cmd_empty_trash(rest),
        "undo" => cmd_undo(rest),
        "dupes" => cmd_dupes(rest),
        "dedupe" => cmd_dedupe(rest),
        "tags" => cmd_tags(rest),
        "tag-rename" => cmd_tag_rename(rest),
        "tag-delete" => cmd_tag_delete(rest),
        "tag-merge" => cmd_tag_merge(rest),
        "search-tags" => cmd_search_tags(rest),
        "playlist" => cmd_playlist(rest),
        "playlist-add" => cmd_playlist_add(rest),
        "playlist-remove" => cmd_playlist_remove(rest),
        "playlist-move" => cmd_playlist_move(rest),
        "playlist-reorder" => cmd_playlist_reorder(rest),
        other => Err(format!(
            "unknown command: {other}. Try `soundhub-cli help`."
        )),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        "soundbuch CLI — headless smoke tests for the core library

USAGE:
  soundhub-cli <command> [args...]

COMMANDS:
  init <library_dir>                      Create / open a Library
  import [--keep-duplicates] <library_dir> <file_or_dir>...   Import audio (copy + hash + metadata)
  list <library_dir>                      List ready assets
  search <library_dir> <text>             Full-text search
  tag <library_dir> <asset_id> <tag>      Add a tag (auto-reindexes FTS)
  untag <library_dir> <asset_id> <tag>    Remove a tag
  person <library_dir> <asset_id> <name>  Link a person
  batch-tag <library_dir> <tag> <id>...   Batch-add tag to many assets
  smart <library_dir> <name> <field> <op> <value>
                                          Create a smart collection
                                          field: tag|person|filename|text|sample_rate|has_gps
                                          op:    is|is_not|contains|eq|gte|lte
  members <library_dir> <collection_id>   List collection members
  gps <library_dir>                       List assets with GPS + bbox sample
  info <library_dir>                      Library stats
  trash <library_dir>                     List recycle-bin contents
  delete <library_dir> <asset_id>...      Move assets to recycle bin (soft)
  restore <library_dir> <asset_id>...     Restore from recycle bin
  purge <library_dir> <asset_id>...       Permanently delete trashed assets
  empty-trash <library_dir>               Empty the recycle bin
  undo <library_dir>                      Undo the last reversible action
  dupes <library_dir>                     List in-library duplicate groups
  dedupe <library_dir> <strategy> [hash]  Collapse duplicates
                                          strategy: oldest|newest|lowest-id
                                          optional hash → only that group
  tags <library_dir>                      List tags with usage counts
  tag-rename <library_dir> <tag_id> <new_name>
  tag-delete <library_dir> <tag_id>       Delete tag everywhere (undoable)
  tag-merge <library_dir> <from_id> <to_id>
  search-tags <library_dir> <and|or> <tag>...
                                          Filter by one or more tags
  playlist <library_dir> [create <name> | list | show <id> | rename <id> <name> | delete <id>]
  playlist-add <library_dir> <playlist_id> <asset_id>...
  playlist-remove <library_dir> <playlist_id> <asset_id>
  playlist-move <library_dir> <playlist_id> <from> <to>
  playlist-reorder <library_dir> <playlist_id> <asset_id>...
"
    );
}

fn open(dir: &str) -> Result<(Library, rusqlite::Connection), String> {
    let path = PathBuf::from(dir);
    if !path.join("library.db").exists() {
        return Err(format!(
            "no library.db under {dir}. Run `soundhub-cli init {dir}` first."
        ));
    }
    let library = Library::open(&path).map_err(|e| e.to_string())?;
    let conn = library.open_db().map_err(|e| e.to_string())?;
    Ok((library, conn))
}

fn cmd_init(args: &[String]) -> Result<(), String> {
    let dir = args.first().ok_or("usage: init <library_dir>")?;
    let path = Path::new(dir);
    let already = path.join("library.db").exists();
    let (library, conn) = if already {
        open(dir)?
    } else {
        let lib = Library::create(path).map_err(|e| e.to_string())?;
        let c = lib.open_db().map_err(|e| e.to_string())?;
        (lib, c)
    };
    let count = Repo::new(&conn)
        .count_ready_assets()
        .map_err(|e| e.to_string())?;
    println!(
        "{} library at {} ({} ready assets)",
        if already { "Opened" } else { "Created" },
        library.root.display(),
        count
    );
    Ok(())
}

fn cmd_import(args: &[String]) -> Result<(), String> {
    let mut keep_dups = false;
    let mut rest: &[String] = args;
    if rest.first().map(|s| s.as_str()) == Some("--keep-duplicates") {
        keep_dups = true;
        rest = &rest[1..];
    }
    if rest.len() < 2 {
        return Err("usage: import [--keep-duplicates] <library_dir> <file_or_dir>...".into());
    }
    let (library, conn) = open(&rest[0])?;
    let sources: Vec<PathBuf> = rest[1..].iter().map(PathBuf::from).collect();
    let action = if keep_dups {
        DuplicateAction::ImportAsDuplicate
    } else {
        DuplicateAction::Skip
    };
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&sources, action)
        .map_err(|e| e.to_string())?;
    println!(
        "import done: success={} duplicate={} unsupported={} failed={} cancelled={}",
        result.success, result.duplicate, result.unsupported, result.failed, result.cancelled
    );
    for f in &result.files {
        let mark = match f.outcome {
            ImportOutcome::Imported => "OK  ",
            ImportOutcome::SkippedDuplicate => "DUP ",
            ImportOutcome::SkippedUnsupported => "SKIP",
            ImportOutcome::Failed => "FAIL",
            ImportOutcome::Cancelled => "CANC",
        };
        println!(
            "  [{mark}] {}{}",
            f.source_path,
            f.asset_id
                .as_deref()
                .map(|s| format!(" → {s}"))
                .unwrap_or_default()
        );
        if let Some(e) = &f.error {
            println!("         {e}");
        }
    }
    Ok(())
}

fn cmd_list(args: &[String]) -> Result<(), String> {
    let (library, conn) = open(args.first().ok_or("usage: list <library_dir>")?)?;
    let _ = library;
    let assets = Repo::new(&conn)
        .list_assets(200, 0)
        .map_err(|e| e.to_string())?;
    if assets.is_empty() {
        println!("(no ready assets)");
        return Ok(());
    }
    for a in assets {
        println!(
            "{}\t{}\t{} Hz\t{}\t{}",
            a.id,
            a.filename,
            a.sample_rate
                .map(|s| s.to_string())
                .unwrap_or_else(|| "—".into()),
            a.duration_ms
                .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
                .unwrap_or_else(|| "—".into()),
            a.file_size,
        );
    }
    Ok(())
}

fn cmd_search(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("usage: search <library_dir> <text>".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let text = args[1..].join(" ");
    let q = SearchQuery {
        text: Some(text.clone()),
        limit: 50,
        ..Default::default()
    };
    let hits = search(&conn, &q).map_err(|e| e.to_string())?;
    println!("search {:?} → {} hit(s)", text, hits.len());
    let repo = Repo::new(&conn);
    for id in hits {
        if let Some(a) = repo.get_asset(&id).map_err(|e| e.to_string())? {
            println!("  {}  {}", a.id, a.filename);
        }
    }
    Ok(())
}

fn parse_id(s: &str) -> Result<AssetId, String> {
    AssetId::parse(s).map_err(|e| e.to_string())
}

fn cmd_tag(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("usage: tag <library_dir> <asset_id> <tag>".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let id = parse_id(&args[1])?;
    soundhub_core::undo::add_tag(&conn, &id, &args[2]).map_err(|e| e.to_string())?;
    println!("tagged {} with '{}'", id, args[2]);
    Ok(())
}

fn cmd_untag(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("usage: untag <library_dir> <asset_id> <tag>".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let id = parse_id(&args[1])?;
    let repo = Repo::new(&conn);
    let tag_id: Option<String> = conn
        .query_row("SELECT id FROM tags WHERE name = ?1", [&args[2]], |r| {
            r.get(0)
        })
        .ok();
    let Some(tag_id) = tag_id else {
        println!("tag '{}' not found", args[2]);
        return Ok(());
    };
    let tag_id = RowId::parse(&tag_id).map_err(|e| e.to_string())?;
    soundhub_core::undo::remove_tag(&conn, &id, &tag_id).map_err(|e| e.to_string())?;
    let _ = repo;
    println!("removed tag '{}' from {}", args[2], id);
    Ok(())
}

fn cmd_person(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("usage: person <library_dir> <asset_id> <name>".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let id = parse_id(&args[1])?;
    soundhub_core::undo::add_person(&conn, &id, &args[2]).map_err(|e| e.to_string())?;
    println!("linked person '{}' to {}", args[2], id);
    Ok(())
}

fn cmd_batch_tag(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("usage: batch-tag <library_dir> <tag> <asset_id>...".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let tag = &args[1];
    let ids: Vec<AssetId> = args[2..]
        .iter()
        .map(|s| parse_id(s))
        .collect::<Result<_, _>>()?;
    let n = Repo::new(&conn)
        .batch_add_tag(&ids, tag)
        .map_err(|e| e.to_string())?;
    println!("batch-tagged {n} asset(s) with '{tag}'");
    Ok(())
}

fn cmd_smart(args: &[String]) -> Result<(), String> {
    if args.len() < 5 {
        return Err(
            "usage: smart <library_dir> <name> <field> <op> <value>\n  e.g. smart ./Lib \"HD\" sample_rate gte 48000".into(),
        );
    }
    let (_lib, conn) = open(&args[0])?;
    let name = &args[1];
    let field = match args[2].as_str() {
        "tag" => SmartField::Tag,
        "person" => SmartField::Person,
        "filename" => SmartField::Filename,
        "text" => SmartField::Text,
        "sample_rate" => SmartField::SampleRate,
        "has_gps" => SmartField::HasGps,
        "recorded_after" => SmartField::RecordedAfter,
        "recorded_before" => SmartField::RecordedBefore,
        other => return Err(format!("unknown field: {other}")),
    };
    let op = match args[3].as_str() {
        "is" => SmartOp::Is,
        "is_not" => SmartOp::IsNot,
        "contains" => SmartOp::Contains,
        "eq" => SmartOp::Eq,
        "gte" => SmartOp::Gte,
        "lte" => SmartOp::Lte,
        "neq" => SmartOp::Neq,
        other => return Err(format!("unknown op: {other}")),
    };
    let raw = &args[4];
    let value = match field {
        SmartField::SampleRate => {
            serde_json::json!(raw.parse::<i64>().map_err(|e| e.to_string())?)
        }
        SmartField::HasGps => serde_json::json!(raw != "false"),
        _ => serde_json::json!(raw),
    };
    let rules = SmartRules {
        match_mode: MatchMode::All,
        conditions: vec![SmartCondition { field, op, value }],
    };
    let repo = Repo::new(&conn);
    let c = repo
        .create_smart_collection(name, &rules)
        .map_err(|e| e.to_string())?;
    let members = repo
        .list_collection_assets(&c.id)
        .map_err(|e| e.to_string())?;
    println!(
        "created smart collection '{}' ({}) → {} member(s)",
        c.name,
        c.id,
        members.len()
    );
    for id in members {
        println!("  {id}");
    }
    Ok(())
}

fn cmd_members(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("usage: members <library_dir> <collection_id>".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let cid = RowId::parse(&args[1]).map_err(|e| e.to_string())?;
    let repo = Repo::new(&conn);
    let ids = repo
        .list_collection_assets(&cid)
        .map_err(|e| e.to_string())?;
    println!("{} member(s)", ids.len());
    for id in ids {
        if let Some(a) = repo.get_asset(&id).map_err(|e| e.to_string())? {
            println!("  {}  {}", a.id, a.filename);
        }
    }
    Ok(())
}

fn cmd_gps(args: &[String]) -> Result<(), String> {
    let (_lib, conn) = open(args.first().ok_or("usage: gps <library_dir>")?)?;
    let repo = Repo::new(&conn);
    let pts = repo.list_assets_with_gps().map_err(|e| e.to_string())?;
    println!("{} asset(s) with GPS", pts.len());
    for (a, lat, lon) in &pts {
        println!("  {}  {}\t{:.5}, {:.5}", a.id, a.filename, lat, lon);
    }
    let near = repo
        .assets_in_bbox(-90.0, 90.0, -180.0, 180.0)
        .map_err(|e| e.to_string())?;
    println!("bbox (world) → {} hit(s)", near.len());
    Ok(())
}

fn cmd_info(args: &[String]) -> Result<(), String> {
    let (library, conn) = open(args.first().ok_or("usage: info <library_dir>")?)?;
    let repo = Repo::new(&conn);
    let count = repo.count_ready_assets().map_err(|e| e.to_string())?;
    let tags = repo.list_tags().map_err(|e| e.to_string())?;
    let cols = repo.list_collections().map_err(|e| e.to_string())?;
    let people = repo.list_people().map_err(|e| e.to_string())?;
    let gps = repo.list_assets_with_gps().map_err(|e| e.to_string())?;
    let trash = repo.count_deleted_assets().map_err(|e| e.to_string())?;
    let undo_n = repo.count_undo_entries().map_err(|e| e.to_string())?;
    println!("Library : {}", library.root.display());
    println!("Assets  : {count}");
    println!("Trash   : {trash}");
    println!("Undo    : {undo_n} step(s)");
    println!("Tags    : {}", tags.len());
    println!("People  : {}", people.len());
    println!(
        "Collections: {} ({} smart)",
        cols.len(),
        cols.iter()
            .filter(|c| c.collection_type == CollectionType::Smart)
            .count()
    );
    println!("GPS     : {}", gps.len());
    Ok(())
}

fn cmd_trash(args: &[String]) -> Result<(), String> {
    let (_lib, conn) = open(args.first().ok_or("usage: trash <library_dir>")?)?;
    let repo = Repo::new(&conn);
    let items = repo
        .list_deleted_assets(200, 0)
        .map_err(|e| e.to_string())?;
    if items.is_empty() {
        println!("(recycle bin is empty)");
        return Ok(());
    }
    for a in items {
        println!(
            "{}\t{}\tdeleted {}",
            a.id,
            a.filename,
            a.deleted_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "?".into())
        );
    }
    Ok(())
}

fn cmd_delete(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("usage: delete <library_dir> <asset_id>...".into());
    }
    let (_library, conn) = open(&args[0])?;
    let ids: Vec<AssetId> = args[1..]
        .iter()
        .map(|s| parse_id(s))
        .collect::<Result<_, _>>()?;
    let n = soundhub_core::undo::soft_delete(&conn, &ids).map_err(|e| e.to_string())?;
    println!("moved {n} asset(s) to recycle bin (use `undo` to reverse)");
    Ok(())
}

fn cmd_restore(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("usage: restore <library_dir> <asset_id>...".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let ids: Vec<AssetId> = args[1..]
        .iter()
        .map(|s| parse_id(s))
        .collect::<Result<_, _>>()?;
    let n = soundhub_core::undo::restore(&conn, &ids).map_err(|e| e.to_string())?;
    println!("restored {n} asset(s)");
    Ok(())
}

fn cmd_purge(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("usage: purge <library_dir> <asset_id>...".into());
    }
    let (library, conn) = open(&args[0])?;
    let ids: Vec<AssetId> = args[1..]
        .iter()
        .map(|s| parse_id(s))
        .collect::<Result<_, _>>()?;
    let repo = Repo::new(&conn);
    let mut n = 0u32;
    for id in &ids {
        if let Some(a) = repo.get_asset(id).map_err(|e| e.to_string())? {
            if a.deleted_at.is_some() {
                let abs = library.asset_abspath(&a.library_relpath);
                repo.purge_asset(id, &abs).map_err(|e| e.to_string())?;
                n += 1;
            } else {
                eprintln!("skip {} (not in recycle bin)", id);
            }
        }
    }
    println!("purged {n} asset(s)");
    Ok(())
}

fn cmd_empty_trash(args: &[String]) -> Result<(), String> {
    let (library, conn) = open(args.first().ok_or("usage: empty-trash <library_dir>")?)?;
    let repo = Repo::new(&conn);
    let deleted = repo
        .list_deleted_assets(10_000, 0)
        .map_err(|e| e.to_string())?;
    let mut n = 0u32;
    for a in deleted {
        let abs = library.asset_abspath(&a.library_relpath);
        repo.purge_asset(&a.id, &abs).map_err(|e| e.to_string())?;
        n += 1;
    }
    println!("emptied trash: {n} item(s)");
    Ok(())
}

fn cmd_undo(args: &[String]) -> Result<(), String> {
    let (_lib, conn) = open(args.first().ok_or("usage: undo <library_dir>")?)?;
    let label = soundhub_core::undo::undo_last(&conn).map_err(|e| e.to_string())?;
    match label {
        Some(l) => println!("undid: {l}"),
        None => println!("nothing to undo"),
    }
    Ok(())
}

fn cmd_dupes(args: &[String]) -> Result<(), String> {
    let (_lib, conn) = open(args.first().ok_or("usage: dupes <library_dir>")?)?;
    let groups = soundhub_core::duplicates::find_groups(&conn).map_err(|e| e.to_string())?;
    if groups.is_empty() {
        println!("(no in-library duplicates)");
        return Ok(());
    }
    println!("{} duplicate group(s):", groups.len());
    for g in groups {
        println!(
            "  {} × {}  {:.1} KB  hash={}",
            g.assets.len(),
            g.assets[0].filename,
            g.file_size as f64 / 1024.0,
            &g.hash[..12.min(g.hash.len())]
        );
        for a in &g.assets {
            println!(
                "      {}  {}  imported {}",
                a.id,
                a.filename,
                a.imported_at.to_rfc3339()
            );
        }
    }
    Ok(())
}

fn cmd_dedupe(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("usage: dedupe <library_dir> <oldest|newest|lowest-id> [hash]".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let strategy = match args[1].as_str() {
        "oldest" => soundhub_core::duplicates::KeepStrategy::Oldest,
        "newest" => soundhub_core::duplicates::KeepStrategy::Newest,
        "lowest-id" => soundhub_core::duplicates::KeepStrategy::LowestId,
        other => return Err(format!("unknown strategy: {other}")),
    };
    if let Some(hash) = args.get(2) {
        let n = soundhub_core::duplicates::dedupe_group(&conn, hash, strategy)
            .map_err(|e| e.to_string())?;
        println!("deduped group: trashed {n} copy(ies) to recycle bin");
    } else {
        let (g, t) =
            soundhub_core::duplicates::dedupe(&conn, strategy).map_err(|e| e.to_string())?;
        println!("deduped {g} group(s): trashed {t} copy(ies) to recycle bin");
    }
    Ok(())
}

fn cmd_tags(args: &[String]) -> Result<(), String> {
    let (_lib, conn) = open(args.first().ok_or("usage: tags <library_dir>")?)?;
    let usage = Repo::new(&conn)
        .list_tags_with_usage()
        .map_err(|e| e.to_string())?;
    if usage.is_empty() {
        println!("(no tags)");
        return Ok(());
    }
    for u in usage {
        println!("{}\t{}\t{} asset(s)", u.id, u.name, u.asset_count);
    }
    Ok(())
}

fn cmd_tag_rename(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("usage: tag-rename <library_dir> <tag_id> <new_name>".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let tid = RowId::parse(&args[1]).map_err(|e| e.to_string())?;
    let old = soundhub_core::undo::rename_tag(&conn, &tid, &args[2]).map_err(|e| e.to_string())?;
    println!("renamed “{old}” → “{}”", args[2]);
    Ok(())
}

fn cmd_tag_delete(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("usage: tag-delete <library_dir> <tag_id>".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let tid = RowId::parse(&args[1]).map_err(|e| e.to_string())?;
    let name = soundhub_core::undo::delete_tag(&conn, &tid).map_err(|e| e.to_string())?;
    println!("deleted tag “{name}” (undo restores it)");
    Ok(())
}

fn cmd_tag_merge(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("usage: tag-merge <library_dir> <from_id> <to_id>".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let from = RowId::parse(&args[1]).map_err(|e| e.to_string())?;
    let to = RowId::parse(&args[2]).map_err(|e| e.to_string())?;
    soundhub_core::undo::merge_tags(&conn, &from, &to).map_err(|e| e.to_string())?;
    println!("merged {from} into {to}");
    Ok(())
}

fn cmd_search_tags(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("usage: search-tags <library_dir> <and|or> <tag>...".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let tags_all = match args[1].as_str() {
        "and" | "all" => true,
        "or" | "any" => false,
        other => return Err(format!("expected and|or, got {other}")),
    };
    let q = soundhub_core::search::SearchQuery {
        tags: args[2..].to_vec(),
        tags_all,
        limit: 100,
        ..Default::default()
    };
    let hits = soundhub_core::search::search(&conn, &q).map_err(|e| e.to_string())?;
    println!("{} hit(s)", hits.len());
    let repo = Repo::new(&conn);
    for id in hits {
        if let Some(a) = repo.get_asset(&id).map_err(|e| e.to_string())? {
            println!("  {}  {}", a.id, a.filename);
        }
    }
    Ok(())
}

fn cmd_playlist(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: playlist <library_dir> <create|list|show|rename|delete> ...".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let repo = Repo::new(&conn);
    match args.get(1).map(|s| s.as_str()) {
        Some("create") => {
            let name = args.get(2).ok_or("usage: playlist <lib> create <name>")?;
            let p = repo.create_playlist(name).map_err(|e| e.to_string())?;
            println!("created playlist {} ({})", p.name, p.id);
        }
        Some("list") | None => {
            let all = repo.list_playlists().map_err(|e| e.to_string())?;
            if all.is_empty() {
                println!("(no playlists)");
            }
            for p in all {
                println!("{}\t{}\t{} track(s)", p.id, p.name, p.track_count);
            }
        }
        Some("show") => {
            let id =
                RowId::parse(args.get(2).ok_or("need playlist_id")?).map_err(|e| e.to_string())?;
            let tracks = repo.list_playlist_tracks(&id).map_err(|e| e.to_string())?;
            println!("{} track(s):", tracks.len());
            for t in tracks {
                println!("  {}. {}\t{}", t.position + 1, t.filename, t.asset_id);
            }
        }
        Some("rename") => {
            let id =
                RowId::parse(args.get(2).ok_or("need playlist_id")?).map_err(|e| e.to_string())?;
            let name = args.get(3).ok_or("need new name")?;
            repo.rename_playlist(&id, name).map_err(|e| e.to_string())?;
            println!("renamed to {name}");
        }
        Some("delete") => {
            let id =
                RowId::parse(args.get(2).ok_or("need playlist_id")?).map_err(|e| e.to_string())?;
            repo.delete_playlist(&id).map_err(|e| e.to_string())?;
            println!("deleted playlist");
        }
        Some(other) => return Err(format!("unknown subcommand: {other}")),
    }
    Ok(())
}

fn cmd_playlist_add(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("usage: playlist-add <library_dir> <playlist_id> <asset_id>...".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let repo = Repo::new(&conn);
    let pid = RowId::parse(&args[1]).map_err(|e| e.to_string())?;
    for s in &args[2..] {
        let aid = parse_id(s)?;
        let pos = repo
            .playlist_add_track(&pid, &aid)
            .map_err(|e| e.to_string())?;
        println!("added {} at position {}", aid, pos);
    }
    Ok(())
}

fn cmd_playlist_remove(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("usage: playlist-remove <library_dir> <playlist_id> <asset_id>".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let repo = Repo::new(&conn);
    let pid = RowId::parse(&args[1]).map_err(|e| e.to_string())?;
    let aid = parse_id(&args[2])?;
    let ok = repo
        .playlist_remove_track(&pid, &aid)
        .map_err(|e| e.to_string())?;
    println!("{}", if ok { "removed" } else { "not in playlist" });
    Ok(())
}

fn cmd_playlist_move(args: &[String]) -> Result<(), String> {
    if args.len() < 4 {
        return Err("usage: playlist-move <library_dir> <playlist_id> <from> <to>".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let repo = Repo::new(&conn);
    let pid = RowId::parse(&args[1]).map_err(|e| e.to_string())?;
    let from: u32 = args[2]
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    let to: u32 = args[3]
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    repo.playlist_move_track(&pid, from, to)
        .map_err(|e| e.to_string())?;
    println!("moved {from} → {to}");
    Ok(())
}

fn cmd_playlist_reorder(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("usage: playlist-reorder <library_dir> <playlist_id> <asset_id>...".into());
    }
    let (_lib, conn) = open(&args[0])?;
    let repo = Repo::new(&conn);
    let pid = RowId::parse(&args[1]).map_err(|e| e.to_string())?;
    let ids: Vec<AssetId> = args[2..]
        .iter()
        .map(|s| parse_id(s))
        .collect::<Result<_, _>>()?;
    repo.playlist_reorder(&pid, &ids)
        .map_err(|e| e.to_string())?;
    println!("reordered {} track(s)", ids.len());
    Ok(())
}

// Silence unused import warning for evaluate when not used in all paths.
#[allow(dead_code)]
fn _touch_evaluate(
    conn: &rusqlite::Connection,
    rules: &SmartRules,
) -> soundhub_core::Result<Vec<AssetId>> {
    let _ = collections::rules_to_value(rules);
    evaluate(conn, rules)
}
