# soundbuch

Local sound asset management for desktop (Windows / macOS / Linux).

soundbuch imports recordings from disks, USB drives, SD cards and network mounts into a single **Library**, preserves the original bytes, and organizes them with metadata, tags, people, and virtual Collections.

> **Core principle:** soundbuch manages sound assets but never destroys or modifies the original audio.

---

## Architecture

```
Original Audio  →  Asset  →  Metadata  →  Relationships  →  Index
```

| Layer | Crate / Path | Responsibility |
|---|---|---|
| Core engine | `crates/soundhub-core` | Library layout, SQLite schema, import pipeline, metadata extraction, search |
| Desktop shell | `src-tauri` | Tauri commands, dialogs, app state |
| UI | `src/` | Static HTML/CSS/JS (no bundler required) |

### Library on disk

```
<Library>/
├── library.db          # SQLite (WAL) — assets, jobs, tags, people, collections, FTS
├── assets/
│   └── ab/sh_01J….wav  # asset_id + original extension (never filename)
├── metadata/           # reserved for sidecars
└── cache/              # reserved for waveforms
```

Virtual Collections never create directories and never copy files. One Asset can belong to N Collections.

---

## Import pipeline (transactional)

```
scan → hash (SHA-256) → create pending asset → copy → verify hash → metadata → mark ready
```

- **Copy, never Move.** Source files are untouched.
- **SHA-256** of source must match the library copy before `status = ready`.
- **Duplicates** are detected by content hash. Default action is **Skip**.
- **One failed file never aborts the job.** Counts: imported / duplicate / unsupported / failed.
- **Crash recovery:** incomplete jobs and non-ready assets are detected on startup and can be **Resumed**.

### Asset identity

Assets are identified by `sh_<ULID>` (e.g. `sh_01JXXXXXXXXXXXX`), never by filename.
Two devices can both produce `0001.wav` — they stay distinct.

---

## Metadata model

| Category | Examples | Storage |
|---|---|---|
| Filesystem | filename, size, times, original_path, source_volume | asset columns |
| Technical | duration, sample_rate, bit_depth, channels, codec, container | asset columns |
| BWF / iXML | Originator, UMID, CodingHistory, scene, GPS | `raw_metadata` JSON + `metadata_values` with provenance |
| User | tags, people, collections, notes | relationship tables |

Unknown fields are **never dropped** — they land in `raw_metadata`.
Values carry `metadata_source` (`filesystem` / `bwf` / `ixml` / `user` / …). User edits never overwrite originals.

---

## MVP scope (this tree)

- [x] Create / open Library
- [x] Import files & folders (recursive)
- [x] SHA-256 hash + copy verification
- [x] Duplicate detection (Skip / Import as duplicate / Cancel)
- [x] Transactional import + crash recovery (Resume / Retry / Clean Up)
- [x] WAV / AIFF / FLAC technical metadata; BWF + iXML extraction; raw preserved
- [x] MP3 (MPEG frame + ID3v1/v2 text) / M4A (mvhd/mp4a) / CAF (desc) technical metadata
- [x] Asset list + detail (id, hash, technical params, people, tags, collections)
- [x] Tags, People, Collections (static, multi-membership; add + remove links)
- [x] Sidebar lists + click-to-filter by tag / collection
- [x] Detail panel: add/remove tag / person / collection
- [x] Full-text search (FTS5) + basic filters (sample rate, tag, collection)
- [x] Minimal desktop UI (import dialog, browser, detail, recovery)
- [x] Async import / scan / resume (UI stays responsive) + live progress events
- [x] Audio playback + waveform (play/pause/seek; peaks cached under `cache/`)

### Not yet (later phases)

- Map / GPS tracks (basic scatter map + bbox filter done; no tile basemap)
- Library backup / migration UI
- Advanced search (OR / NOT), saved searches
- Full OGG / Opus / WMA metadata parsers

### Phase 2 (added)

- [x] **Smart Collections** — rule-driven membership (`tag` / `person` / `filename` / `text` / `sample_rate` / `has_gps` / `recorded_after`), `all`/`any` match, evaluated live (no membership table)
- [x] **Map / GPS** — scatter plot of GPS-tagged assets, click-to-select, bbox query API
- [x] **Batch metadata editing** — multi-select rows, batch add/remove tag, add person, add/remove collection

---

## Development

**Main development environment:** Ubuntu (Dell 7865)  
**Supported desktop platforms:** Windows / macOS / Linux

The repository is a Rust workspace with a platform-agnostic core and a Tauri 2
desktop shell. The frontend is static HTML/CSS/JS — no bundler, no Node build.

### Ubuntu local development

```bash
# one-time system packages (Tauri / WebKit)
sudo apt install libdbus-1-dev libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev pkg-config

# tests
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets

# run the desktop app (debug)
cargo run -p soundhub

# headless CLI (cross-platform binary)
cargo run -p soundhub-cli -- help
cargo run -p soundhub-cli -- init ./my-library
cargo run -p soundhub-cli -- import ./my-library ./some-wavs
```

### Linux/macOS development helpers

These scripts are **not** used by CI and are not required on Windows:

| Script | Purpose |
|--------|---------|
| `scripts/run-local.sh` | build + launch desktop app |
| `scripts/smoke-test.sh` | end-to-end CLI smoke test |
| `scripts/make-samples.py` | generate small WAV files |

On Windows, use the `cargo run -p soundhub-cli` commands above instead.

### GitHub Actions (test + build)

```
git push
   ↓
GitHub Actions  (fmt → clippy → test → tauri build)
   ↓
Artifacts: soundbuch-linux-x64 / soundbuch-windows-x64 / soundbuch-macos-arm64
```

Workflow: `.github/workflows/build.yml`  
Triggers: `push` to `main`, or `workflow_dispatch` (Actions → CI → Run workflow).

**Download a test build (no Rust/Node required for testers):**

1. Open https://github.com/cosmosdeng/soundbuch/actions
2. Click the latest successful **CI** run
3. Download the artifact for your OS:
   - Windows → `soundbuch-windows-x64`
   - Mac (Apple Silicon) → `soundbuch-macos-arm64`
   - Linux → `soundbuch-linux-x64`
4. Unzip and install/run.

**macOS note:** these are **unsigned test builds**. Gatekeeper may block the app.

> This is an unsigned test build.  
> macOS may require the user to approve the application in **Privacy & Security**  
> or open it manually through **Finder** (right-click → Open).

Apple Developer ID signing / notarization is intentionally not configured yet.

### Workspace layout

```
soundbuch/
├── Cargo.toml                 # workspace
├── crates/soundhub-core/      # pure cross-platform logic (no Tauri/GUI)
│   ├── src/{audio,db,duplicates,import,library,metadata,models,search,undo}
│   └── tests/
├── crates/soundhub-cli/       # headless CLI for scripts and CI-friendly tests
├── src-tauri/                 # Tauri 2 desktop shell (platform glue)
├── src/                       # static frontend (index.html / main.js / styles.css)
└── .github/workflows/build.yml
```

### Cross-platform notes

- Library layout is portable: stored relative paths always use `/` and are
  resolved with `Path::join` (works on Windows, macOS, Linux).
- External volumes (`D:\`, `/Volumes/…`, `/media/…`, `/mnt/…`, network mounts)
  are supported as import source and Library location.
- Unicode filenames (CJK, spaces, hyphens, long names) are covered by tests.
- No external CLI tools (ffmpeg/ffprobe) are required at runtime.

---

## Acceptance mapping (PRD §51)

| Test | Covered by |
|---|---|
| 01 First launch | UI setup screen (`create_library` / `open_library`) |
| 02 Single file | `tests/import_pipeline.rs::test_02_single_file_import` |
| 03 Folder of 100 | `test_03_folder_with_many_files` |
| 04 Subdirectories | `test_04_recursive_subdirectories` |
| 05 Duplicate | `test_05_duplicate_detection_by_sha256` |
| 06 USB removal | `test_06_external_source_can_be_removed_after_import` |
| 07 Metadata BWF/iXML | `metadata::bwf` unit tests |
| 08 Collections share file | `tests/organization_search.rs::test_08_…` |
| 09 Search | `test_09_search_by_filename_tag_person_location` |
| 10 Crash recovery | `test_10_crash_leaves_recoverable_state…` |

---

## License

MIT
