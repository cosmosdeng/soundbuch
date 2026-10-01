// soundbuch UI — talks to the Tauri backend via `invoke`.

const rawInvoke = window.__TAURI__?.core?.invoke
  ? window.__TAURI__.core.invoke.bind(window.__TAURI__.core)
  : window.__TAURI_INTERNALS__?.invoke;

async function invoke(cmd, args) {
  if (!rawInvoke) throw new Error("Tauri invoke not available");
  return rawInvoke(cmd, args ?? {});
}

if (!rawInvoke) {
  document.body.innerHTML =
    '<p style="padding:2rem">soundbuch must be launched as a desktop app (Tauri).</p>';
  throw new Error("Tauri invoke not available");
}

const tauriEvent = window.__TAURI__?.event;
const tauriDialog = window.__TAURI__?.dialog;

function log(...args) {
  console.log("[soundbuch]", ...args);
}

function setMsg(id, text, kind) {
  const el = $(id);
  if (!el) return;
  el.textContent = text || "";
  el.className = "status-msg" + (kind ? " " + kind : "");
}

// Live progress from async import/resume/retry (rule 12: UI never freezes).
if (tauriEvent?.listen) {
  tauriEvent.listen("import-progress", (e) => {
    const p = e.payload || {};
    log("progress", p);
    const el = $("import-progress");
    if (!el || el.classList.contains("hidden")) return;
    const pct = p.total > 0 ? Math.round((p.processed / p.total) * 100) : 0;
    el.innerHTML = `
      <div class="progress-bar"><div class="progress-fill" style="width:${pct}%"></div></div>
      <div class="muted">${escapeHtml(p.kind || "")}: ${p.processed}/${p.total} — ${escapeHtml(p.current || "")}</div>
    `;
  });
}

const $ = (id) => document.getElementById(id);

const state = {
  assets: [],
  selectedId: null,
  pendingPaths: [],
  currentView: "all",
  filter: { tag: null, collectionId: null, tags: [], tagsAll: true },
  tags: [],
  collections: [],
  multiSelect: false,
  selectedSet: new Set(),
  player: {
    audio: null,
    revoke: null,
    peaks: null,
    raf: 0,
  },
};

// ── i18n (MVP: English + 简体中文) ─────────────────────────────────────────

const I18N = {
  en: {
    tagline: "Sound asset library",
    setupTitle: "Where is your Library?",
    setupLead:
      "soundbuch keeps your recordings in one place. Original files are copied in — never modified.",
    createTitle: "Create new Library",
    createDesc: "Pick a folder on an internal disk, SSD, or external drive.",
    createLead: "Choose where to store your sound assets.",
    openTitle: "Open existing Library",
    openDesc: "Open a Library you created earlier (contains library.db).",
    openLead: "Select the folder that contains your soundbuch Library.",
    recentTitle: "Recent Libraries",
    noRecent: "No recent libraries yet.",
    parentFolder: "Parent folder",
    libraryFolder: "Library folder",
    libraryName: "Library name",
    nameHint: "Leave empty to use the folder itself as the Library.",
    browse: "Browse…",
    willCreate: "Will create",
    createConfirm: "Create Library",
    openConfirm: "Open Library",
    back: "Back",
    status: "Status",
    assets: "Assets",
    database: "Database",
    validLibrary: "Valid soundbuch Library",
    notLibrary: "Not a soundbuch Library",
    alreadyExists: "A Library already exists at this location. Use Open instead.",
    emptyFolderOk: "Folder is empty — ready to create a Library.",
    folderNotEmpty: "Folder is not empty and has no library.db.",
    creating: "Creating…",
    opening: "Opening…",
    createOk: "Library created.",
    openOk: "Library opened.",
    forget: "Remove",
    suggested: "Suggested",
    assetCount: (n) => `${n} assets`,
    dbSize: (n) => `${(n / 1024).toFixed(1)} KB`,
  },
  zh: {
    tagline: "声音资产库",
    setupTitle: "Library 在哪里？",
    setupLead:
      "soundbuch 将录音集中管理。文件会复制进 Library，原始录音不会被修改。",
    createTitle: "创建新 Library",
    createDesc: "选择内置硬盘、SSD 或移动硬盘上的文件夹。",
    createLead: "选择声音资产的存放位置。",
    openTitle: "打开已有 Library",
    openDesc: "打开此前创建过的 Library（包含 library.db）。",
    openLead: "请选择包含 soundbuch Library 的文件夹。",
    recentTitle: "最近打开",
    noRecent: "暂无最近记录。",
    parentFolder: "上级文件夹",
    libraryFolder: "Library 文件夹",
    libraryName: "Library 名称",
    nameHint: "留空则直接把所选文件夹作为 Library。",
    browse: "浏览…",
    willCreate: "将创建",
    createConfirm: "创建 Library",
    openConfirm: "打开 Library",
    back: "返回",
    status: "状态",
    assets: "资产数",
    database: "数据库",
    validLibrary: "有效的 soundbuch Library",
    notLibrary: "不是 soundbuch Library",
    alreadyExists: "此位置已存在 Library，请改用「打开」。",
    emptyFolderOk: "文件夹为空，可以创建 Library。",
    folderNotEmpty: "文件夹非空，且没有 library.db。",
    creating: "创建中…",
    opening: "打开中…",
    createOk: "Library 已创建。",
    openOk: "Library 已打开。",
    forget: "移除",
    suggested: "推荐位置",
    assetCount: (n) => `${n} 个资产`,
    dbSize: (n) => `${(n / 1024).toFixed(1)} KB`,
  },
};

let lang = localStorage.getItem("soundhub_lang") || "en";

function t(key) {
  const v = I18N[lang]?.[key] ?? I18N.en[key] ?? key;
  return typeof v === "function" ? v : v;
}

function applyI18n() {
  document.querySelectorAll("[data-i18n]").forEach((el) => {
    const key = el.getAttribute("data-i18n");
    const val = t(key);
    if (typeof val === "function") return;
    el.textContent = val;
  });
  document.querySelectorAll(".lang-btn").forEach((b) => {
    b.classList.toggle("active", b.dataset.lang === lang);
  });
  // Re-render dynamic bits
  renderRecent();
  updateCreatePreview();
  updateOpenInspection();
}

document.querySelectorAll(".lang-btn").forEach((b) => {
  b.addEventListener("click", () => {
    lang = b.dataset.lang;
    localStorage.setItem("soundhub_lang", lang);
    applyI18n();
  });
});

// ── Library setup ──────────────────────────────────────────────────────────

const setup = {
  step: "choose", // choose | create | open
  createParent: "",
  createName: "soundbuch",
  openPath: "",
  openInspect: null,
};

function setSetupStep(step) {
  setup.step = step;
  $("setup-step-choose").classList.toggle("hidden", step !== "choose");
  $("setup-step-create").classList.toggle("hidden", step !== "create");
  $("setup-step-open").classList.toggle("hidden", step !== "open");
}

document.querySelectorAll("[data-back]").forEach((el) => {
  el.addEventListener("click", () => setSetupStep(el.dataset.back));
});

$("choice-create").addEventListener("click", () => {
  setSetupStep("create");
  updateCreatePreview();
  renderSuggestLocations();
});
$("choice-open").addEventListener("click", () => {
  setSetupStep("open");
  updateOpenInspection();
});

// Suggested parent folders (Documents / Home / D:\ …)
async function renderSuggestLocations() {
  const row = $("suggest-locations");
  if (!row) return;
  let items = [];
  try {
    items = await invoke("suggest_library_locations");
  } catch (_) {
    items = [];
  }
  if (!items.length) {
    row.innerHTML = "";
    return;
  }
  row.innerHTML = items
    .map(
      (loc) => `
      <button type="button" class="suggest-chip" data-path="${escapeAttr(loc.path)}">
        <span class="sc-label">${escapeHtml(loc.label)}</span>
        <span class="sc-path">${escapeHtml(loc.path)}</span>
      </button>`
    )
    .join("");
  row.querySelectorAll(".suggest-chip").forEach((btn) => {
    btn.addEventListener("click", async () => {
      setup.createParent = btn.dataset.path;
      $("create-parent").value = btn.dataset.path;
      await updateCreatePreview();
    });
  });
}

async function tryRestore() {
  try {
    const info = await invoke("library_info");
    if (info) {
      showApp(info);
      await refresh();
      await checkRecovery();
      return;
    }
  } catch (_) {
    /* not open yet */
  }
  $("setup-screen").classList.remove("hidden");
  setSetupStep("choose");
  applyI18n();
  await renderRecent();
}

async function pickDirectory(title) {
  // 1) Tauri dialog plugin
  try {
    if (tauriDialog?.open) {
      const p = await tauriDialog.open({ directory: true, multiple: false, title });
      if (typeof p === "string" && p) return p;
      if (Array.isArray(p) && p.length) return p[0];
      if (p == null) return null; // cancelled
    }
  } catch (e) {
    log("dialog.open directory failed", e);
  }
  // 2) backend rfd command
  try {
    const p = await invoke("pick_directory", { title: title ?? null });
    if (p) return p;
    return null;
  } catch (e) {
    log("pick_directory failed", e);
    return window.prompt(title || "Folder path:") || null;
  }
}

async function pickFiles() {
  try {
    if (tauriDialog?.open) {
      const files = await tauriDialog.open({
        multiple: true,
        title: "Select audio files",
        filters: [
          {
            name: "Audio",
            extensions: ["wav", "bwf", "aif", "aiff", "flac", "mp3", "m4a", "aac", "caf"],
          },
        ],
      });
      if (Array.isArray(files) && files.length) return files;
      if (typeof files === "string" && files) return [files];
      if (files == null) return null;
    }
  } catch (e) {
    log("dialog.open files failed", e);
  }
  try {
    const files = await invoke("pick_audio_files", {});
    return files && files.length ? files : null;
  } catch (e) {
    log("pick_audio_files failed", e);
    return null;
  }
}

async function pickFolder() {
  return await pickDirectory("Select folder to import");
}

// Recent libraries
async function renderRecent() {
  const block = $("recent-block");
  const list = $("recent-list");
  let items = [];
  try {
    items = await invoke("recent_libraries");
  } catch (_) {
    items = [];
  }
  if (!items.length) {
    block.classList.add("hidden");
    list.innerHTML = "";
    return;
  }
  block.classList.remove("hidden");
  list.innerHTML = items
    .map(
      (r) => `
      <li class="recent-item" data-path="${escapeAttr(r.path)}">
        <div class="ri-path" title="${escapeAttr(r.path)}">${escapeHtml(r.path)}</div>
        <div class="ri-meta">${
          r.asset_count != null ? t("assetCount")(r.asset_count) : ""
        }</div>
        <button type="button" class="ri-forget" data-forget="${escapeAttr(r.path)}" title="${t("forget")}">×</button>
      </li>`
    )
    .join("");

  list.querySelectorAll(".recent-item").forEach((li) => {
    li.addEventListener("click", async (e) => {
      if (e.target.dataset.forget) return;
      const path = li.dataset.path;
      await openLibraryAt(path);
    });
  });
  list.querySelectorAll("[data-forget]").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      await invoke("forget_recent_library", { path: btn.dataset.forget });
      await renderRecent();
    });
  });
}

function escapeHtml(s) {
  return String(s)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}
function escapeAttr(s) {
  return escapeHtml(s).replaceAll("'", "&#39;");
}

// Create flow
$("btn-pick-parent").addEventListener("click", async () => {
  const p = await pickDirectory(t("parentFolder"));
  if (!p) return;
  setup.createParent = p;
  $("create-parent").value = p;
  await updateCreatePreview();
});

$("create-name").addEventListener("input", async (e) => {
  setup.createName = e.target.value;
  await updateCreatePreview();
});

async function updateCreatePreview() {
  const parent = setup.createParent;
  const name = setup.createName || "";
  const fullEl = $("create-fullpath");
  const previewEl = $("create-preview");
  const warnEl = $("create-warning");
  const btn = $("btn-confirm-create");

  if (!parent) {
    fullEl.textContent = "—";
    previewEl.innerHTML = '<li class="muted">—</li>';
    warnEl.classList.add("hidden");
    btn.disabled = true;
    return;
  }

  try {
    const full = await invoke("compose_library_path", {
      parent,
      name,
    });
    fullEl.textContent = full;
    const files = await invoke("creation_preview", { path: full });
    previewEl.innerHTML = files.map((f) => `<li>${escapeHtml(f)}</li>`).join("");

    const inspect = await invoke("inspect_library_path", { path: full });
    if (inspect.is_library) {
      warnEl.textContent = t("alreadyExists");
      warnEl.classList.remove("hidden", "info");
      warnEl.classList.add("warn");
      btn.disabled = true;
    } else if (inspect.exists && inspect.is_directory) {
      // Non-empty folder without db — allow but note it.
      // We don't have entry count; show gentle info.
      warnEl.textContent = t("emptyFolderOk");
      warnEl.classList.add("info");
      warnEl.classList.remove("hidden");
      btn.disabled = false;
    } else if (!inspect.exists) {
      warnEl.classList.add("hidden");
      btn.disabled = false;
    } else {
      warnEl.textContent = inspect.error || t("folderNotEmpty");
      warnEl.classList.remove("hidden", "info");
      btn.disabled = true;
    }
  } catch (e) {
    fullEl.textContent = String(e);
    btn.disabled = true;
  }
}

$("btn-confirm-create").addEventListener("click", async () => {
  const parent = setup.createParent;
  const name = setup.createName || "";
  if (!parent) return;
  const status = $("create-status");
  status.textContent = t("creating");
  status.className = "status-msg";
  $("btn-confirm-create").disabled = true;
  try {
    const full = await invoke("compose_library_path", { parent, name });
    const info = await invoke("create_library", { path: full });
    status.textContent = t("createOk");
    status.className = "status-msg ok";
    showApp(info);
    await refresh();
    await checkRecovery();
  } catch (e) {
    status.textContent = String(e);
    status.className = "status-msg error";
    $("btn-confirm-create").disabled = false;
  }
});

// Open flow
$("btn-pick-open").addEventListener("click", async () => {
  const p = await pickDirectory(t("libraryFolder"));
  if (!p) return;
  setup.openPath = p;
  $("open-path").value = p;
  await updateOpenInspection();
});

async function updateOpenInspection() {
  const path = setup.openPath;
  const box = $("open-inspect");
  const statusEl = $("open-status");
  const assetsEl = $("open-assets");
  const dbEl = $("open-db");
  const errEl = $("open-error");
  const btn = $("btn-confirm-open");

  if (!path) {
    box.classList.add("hidden");
    btn.disabled = true;
    return;
  }

  box.classList.remove("hidden");
  try {
    const insp = await invoke("inspect_library_path", { path });
    setup.openInspect = insp;

    if (insp.is_library) {
      statusEl.textContent = t("validLibrary");
      statusEl.className = "inspect-value ok";
      assetsEl.textContent =
        insp.asset_count != null ? t("assetCount")(insp.asset_count) : "—";
      dbEl.textContent = insp.db_size_bytes != null ? t("dbSize")(insp.db_size_bytes) : "—";
      errEl.classList.add("hidden");
      btn.disabled = false;
    } else {
      statusEl.textContent = t("notLibrary");
      statusEl.className = "inspect-value bad";
      assetsEl.textContent = "—";
      dbEl.textContent = "—";
      errEl.textContent = insp.error || t("notLibrary");
      errEl.classList.remove("hidden");
      btn.disabled = true;
    }
  } catch (e) {
    statusEl.textContent = t("notLibrary");
    statusEl.className = "inspect-value bad";
    errEl.textContent = String(e);
    errEl.classList.remove("hidden");
    btn.disabled = true;
  }
}

$("btn-confirm-open").addEventListener("click", async () => {
  if (!setup.openPath) return;
  await openLibraryAt(setup.openPath);
});

async function openLibraryAt(path) {
  const status = $("open-status-msg");
  if (status) {
    status.textContent = t("opening");
    status.className = "status-msg";
  }
  try {
    const info = await invoke("open_library", { path });
    if (status) {
      status.textContent = t("openOk");
      status.className = "status-msg ok";
    }
    showApp(info);
    await refresh();
    await checkRecovery();
  } catch (e) {
    if (status) {
      status.textContent = String(e);
      status.className = "status-msg error";
    }
  }
}

function showApp(info) {
  $("setup-screen").classList.add("hidden");
  $("app-screen").classList.remove("hidden");
  $("library-label").textContent = info.root;
  $("asset-count").textContent = `${info.asset_count} assets`;
}

// ── Sidebar: tags & collections ────────────────────────────────────────────

async function refreshSidebar() {
  try {
    [state.tags, state.collections] = await Promise.all([
      invoke("list_tags_with_usage"),
      invoke("list_collections"),
    ]);
  } catch (_) {
    state.tags = [];
    state.collections = [];
  }
  renderSidebarChips();
}

function renderSidebarChips() {
  const tagList = $("tag-list");
  const colList = $("collection-list");
  if (tagList) {
    tagList.innerHTML = state.tags.length
      ? state.tags
          .map((t) => {
            const on = state.filter.tags.includes(t.name) || state.filter.tag === t.name;
            const n = t.asset_count != null ? ` (${t.asset_count})` : "";
            return `<span class="chip${on ? " active" : ""}" data-tag="${escapeAttr(t.name)}">${escapeHtml(t.name)}${n}</span>`;
          })
          .join("")
      : '<span class="muted" style="font-size:12px">—</span>';
    tagList.querySelectorAll("[data-tag]").forEach((el) => {
      el.addEventListener("click", (ev) => {
        const name = el.dataset.tag;
        // Click = toggle membership in the multi-tag filter set.
        const set = new Set(state.filter.tags);
        if (set.has(name)) set.delete(name);
        else set.add(name);
        state.filter.tags = [...set];
        state.filter.tag = state.filter.tags.length === 1 ? state.filter.tags[0] : null;
        state.filter.collectionId = null;
        renderSidebarChips();
        refresh();
      });
    });
  }
  if (colList) {
    colList.innerHTML = state.collections.length
      ? state.collections
          .map((c) => {
            const badge = c.collection_type === "smart" ? " ⚡" : "";
            return `<span class="chip${state.filter.collectionId === c.id ? " active" : ""}" data-col="${escapeAttr(c.id)}" data-name="${escapeAttr(c.name)}" title="${c.collection_type === "smart" ? "Smart collection" : "Static collection"}">${escapeHtml(c.name)}${badge}</span>`;
          })
          .join("")
      : '<span class="muted" style="font-size:12px">—</span>';
    colList.querySelectorAll("[data-col]").forEach((el) => {
      el.addEventListener("click", () => {
        state.filter.collectionId =
          state.filter.collectionId === el.dataset.col ? null : el.dataset.col;
        state.filter.tag = null;
        state.filter.tags = [];
        renderSidebarChips();
        refresh();
      });
    });
  }
  fillBatchCollections();
  renderTagManager();
}

// ── Assets list / detail ───────────────────────────────────────────────────

async function refresh() {
  // Map view loads GPS points instead of the table.
  if (state.currentView === "map") {
    await showMap();
    return;
  }
  $("map-panel")?.classList.add("hidden");
  $("asset-rows")?.parentElement?.classList.remove("hidden");

  // Recycle bin uses a different listing.
  if (state.currentView === "trash") {
    $("trash-bar")?.classList.remove("hidden");
    $("btn-empty-trash")?.classList.remove("hidden");
    $("btn-delete")?.classList.add("hidden");
    $("dupes-bar")?.classList.add("hidden");
    try {
      state.assets = await invoke("list_deleted_assets");
    } catch (e) {
      log("list_deleted_assets failed", e);
      state.assets = [];
    }
    renderTable(state.assets);
    await refreshSidebar();
    return;
  }
  $("trash-bar")?.classList.add("hidden");
  $("btn-empty-trash")?.classList.add("hidden");
  $("btn-delete")?.classList.remove("hidden");

  // Duplicates report view.
  if (state.currentView === "dupes") {
    $("dupes-bar")?.classList.remove("hidden");
    $("btn-delete")?.classList.add("hidden");
    $("playlist-bar")?.classList.add("hidden");
    await showDuplicates();
    return;
  }
  $("dupes-bar")?.classList.add("hidden");

  // Playlists.
  if (state.currentView === "playlists") {
    $("playlist-bar")?.classList.remove("hidden");
    $("btn-delete")?.classList.add("hidden");
    await showPlaylists();
    return;
  }
  $("playlist-bar")?.classList.add("hidden");

  // Smart collections resolve members via rules — not the membership table.
  const col = state.collections.find((c) => c.id === state.filter.collectionId);
  if (col && col.collection_type === "smart") {
    try {
      state.assets = await invoke("list_collection_assets", {
        collectionId: col.id,
      });
      log("list_collection_assets (smart)", state.assets.length);
    } catch (e) {
      log("list_collection_assets failed", e);
      state.assets = [];
    }
    renderTable(state.assets);
    await refreshSidebar();
    return;
  }

  const query = {
    limit: 200,
    text: $("search-input")?.value?.trim() || null,
    tag: state.filter.tag,
    tags: state.filter.tags,
    tags_all: state.filter.tagsAll,
    collection_id: state.filter.collectionId,
  };
  try {
    state.assets = await invoke("search_assets", { query });
    log("search_assets", state.assets.length);
  } catch (e) {
    log("search_assets failed, falling back to list_assets", e);
    try {
      state.assets = await invoke("list_assets", { limit: 200, offset: 0 });
      log("list_assets", state.assets.length);
    } catch (e2) {
      log("list_assets failed", e2);
      state.assets = [];
    }
  }
  renderTable(state.assets);
  try {
    const info = await invoke("library_info");
    if (info) $("asset-count").textContent = `${info.asset_count} assets`;
  } catch (_) {}
  await refreshSidebar();
  await refreshUndoButton();
}

// ── Map view ───────────────────────────────────────────────────────────────

let mapPoints = [];

async function showMap() {
  const table = document.querySelector(".asset-table");
  const empty = $("empty-state");
  const panel = $("map-panel");
  if (table) table.classList.add("hidden");
  if (empty) empty.classList.add("hidden");
  if (panel) panel.classList.remove("hidden");

  try {
    mapPoints = await invoke("list_assets_with_gps");
  } catch (e) {
    log("list_assets_with_gps failed", e);
    mapPoints = [];
  }
  drawMap();
}

function drawMap() {
  const canvas = $("map-canvas");
  if (!canvas) return;
  const ctx = canvas.getContext("2d");
  const W = canvas.width;
  const H = canvas.height;
  ctx.clearRect(0, 0, W, H);
  ctx.fillStyle = "#0b1220";
  ctx.fillRect(0, 0, W, H);

  const legend = $("map-legend");
  if (!mapPoints.length) {
    ctx.fillStyle = "#6b7c93";
    ctx.font = "14px sans-serif";
    ctx.fillText("No GPS-tagged assets yet.", 24, 32);
    if (legend) legend.textContent = "Import recordings with BWF/iXML GPS to see them here.";
    return;
  }

  // Equirectangular scatter; pad so points aren't on the edge.
  const pad = 28;
  const lats = mapPoints.map((p) => p.latitude);
  const lons = mapPoints.map((p) => p.longitude);
  const minLat = Math.min(...lats);
  const maxLat = Math.max(...lats);
  const minLon = Math.min(...lons);
  const maxLon = Math.max(...lons);
  const spanLat = Math.max(maxLat - minLat, 1e-6);
  const spanLon = Math.max(maxLon - minLon, 1e-6);

  // Graticule
  ctx.strokeStyle = "#1a2740";
  ctx.lineWidth = 1;
  for (let i = 0; i <= 4; i++) {
    const y = pad + ((H - 2 * pad) * i) / 4;
    const x = pad + ((W - 2 * pad) * i) / 4;
    ctx.beginPath();
    ctx.moveTo(pad, y);
    ctx.lineTo(W - pad, y);
    ctx.moveTo(x, pad);
    ctx.lineTo(x, H - pad);
    ctx.stroke();
  }

  mapPoints.forEach((p, i) => {
    const x = pad + ((p.longitude - minLon) / spanLon) * (W - 2 * pad);
    // Canvas y grows down; invert latitude.
    const y = H - pad - ((p.latitude - minLat) / spanLat) * (H - 2 * pad);
    p._x = x;
    p._y = y;
    const selected = state.selectedId === p.id;
    ctx.beginPath();
    ctx.arc(x, y, selected ? 8 : 5, 0, Math.PI * 2);
    ctx.fillStyle = selected ? "#5a9dff" : "#3d8bfd";
    ctx.globalAlpha = 0.9;
    ctx.fill();
    ctx.globalAlpha = 1;
    if (mapPoints.length <= 20) {
      ctx.fillStyle = "#9db0c9";
      ctx.font = "11px sans-serif";
      ctx.fillText(p.filename.slice(0, 18), x + 8, y + 4);
    }
  });

  if (legend) {
    legend.textContent = `${mapPoints.length} GPS asset(s) · lat ${minLat.toFixed(4)}…${maxLat.toFixed(4)}, lon ${minLon.toFixed(4)}…${maxLon.toFixed(4)}`;
  }
}

function bindMapClick() {
  const canvas = $("map-canvas");
  if (!canvas || canvas.dataset.bound) return;
  canvas.dataset.bound = "1";
  canvas.addEventListener("click", (ev) => {
    const rect = canvas.getBoundingClientRect();
    const scaleX = canvas.width / rect.width;
    const scaleY = canvas.height / rect.height;
    const x = (ev.clientX - rect.left) * scaleX;
    const y = (ev.clientY - rect.top) * scaleY;
    let best = null;
    let bestD = 18;
    for (const p of mapPoints) {
      const d = Math.hypot((p._x ?? -999) - x, (p._y ?? -999) - y);
      if (d < bestD) {
        bestD = d;
        best = p;
      }
    }
    if (best) {
      selectAsset(best.id);
      drawMap();
    }
  });
}

function fmtDuration(ms) {
  if (ms == null) return "—";
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const pad = (n) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}

function fmtSize(bytes) {
  if (bytes == null) return "—";
  const units = ["B", "KB", "MB", "GB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

function fmtDate(iso) {
  if (!iso) return "—";
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso;
  }
}

function renderTable(assets) {
  const tbody = $("asset-rows");
  tbody.innerHTML = "";
  $("empty-state").classList.toggle("hidden", assets.length > 0);
  document.querySelectorAll(".col-check").forEach((th) => {
    th.classList.toggle("hidden", !state.multiSelect);
  });
  for (const a of assets) {
    const tr = document.createElement("tr");
    tr.dataset.id = a.id;
    if (a.id === state.selectedId) tr.classList.add("selected");
    if (state.selectedSet?.has(a.id)) tr.classList.add("selected");
    const checkTd = state.multiSelect
      ? `<td class="col-check"><input type="checkbox" data-check="${escapeAttr(a.id)}" ${state.selectedSet?.has(a.id) ? "checked" : ""} /></td>`
      : "";
    tr.innerHTML = `
      ${checkTd}
      <td title="${escapeAttr(a.original_path)}">${escapeHtml(a.filename)}</td>
      <td>${fmtDuration(a.duration_ms)}</td>
      <td>${a.sample_rate ? `${a.sample_rate / 1000} kHz` : "—"}${
      a.bit_depth ? ` / ${a.bit_depth}b` : ""
    }${a.channels ? ` / ${a.channels}ch` : ""}</td>
      <td>${escapeHtml(fmtDate(a.recorded_at))}</td>
      <td>${fmtSize(a.file_size)}</td>
    `;
    tr.addEventListener("click", (ev) => {
      if (state.multiSelect && ev.target.matches("input[type=checkbox]")) {
        toggleMulti(a.id, ev.target.checked);
        return;
      }
      if (state.multiSelect) {
        const box = tr.querySelector("input[type=checkbox]");
        if (box) {
          box.checked = !box.checked;
          toggleMulti(a.id, box.checked);
        }
        return;
      }
      selectAsset(a.id);
    });
    tbody.appendChild(tr);
  }
  updateBatchBar();
}

function toggleMulti(id, on) {
  if (!state.selectedSet) state.selectedSet = new Set();
  if (on) state.selectedSet.add(id);
  else state.selectedSet.delete(id);
  updateBatchBar();
}

function updateBatchBar() {
  const bar = $("batch-bar");
  if (!bar) return;
  bar.classList.toggle("hidden", !state.multiSelect);
  const n = state.selectedSet?.size ?? 0;
  const el = $("batch-count");
  if (el) el.textContent = `${n} selected`;
}

async function selectAsset(id) {
  state.selectedId = id;
  stopPlayer();
  renderTable(state.assets);
  const d = await invoke("get_asset", { id });
  renderDetail(d);
}

function renderDetail(d) {
  const a = d.summary;
  const chips = (arr, kind) =>
    arr.length
      ? arr
          .map(
            (t) =>
              `<span class="chip removable" data-kind="${kind}" data-id="${escapeAttr(t.id)}">${escapeHtml(t.name)}<button type="button" class="chip-x" data-kind="${kind}" data-id="${escapeAttr(t.id)}" title="Remove">×</button></span>`
          )
          .join("")
      : '<span class="muted">—</span>';

  $("detail-panel").innerHTML = `
    <div class="player" id="player-box">
      <div class="waveform-wrap" id="waveform-wrap" title="Click to seek">
        <canvas id="waveform"></canvas>
        <div class="waveform-played" id="waveform-played" style="width:0"></div>
      </div>
      <div class="player-controls">
        <button type="button" id="btn-play" class="primary" title="Play">▶</button>
        <span class="time" id="player-time">0:00 / 0:00</span>
      </div>
      <div class="player-note muted" id="player-note"></div>
    </div>
    <h3>${escapeHtml(a.filename)}</h3>
    <div class="row"><div class="label">Duration</div><div class="value">${fmtDuration(a.duration_ms)}</div></div>
    <div class="row"><div class="label">Format</div><div class="value">${
      a.sample_rate ? `${a.sample_rate / 1000} kHz` : "—"
    } / ${a.bit_depth ?? "—"} bit / ${a.channels ?? "—"} ch<br/>${escapeHtml(d.codec ?? "—")} · ${escapeHtml(d.container ?? "—")}</div></div>
    <div class="row"><div class="label">Recorded</div><div class="value">${escapeHtml(fmtDate(a.recorded_at))}</div></div>
    <div class="row"><div class="label">Imported</div><div class="value">${escapeHtml(fmtDate(a.imported_at))}</div></div>
    <div class="row"><div class="label">Location</div><div class="value">${
      d.latitude != null ? `${d.latitude}, ${d.longitude}` : "—"
    }</div></div>
    <div class="row"><div class="label">People</div><div class="chips">${chips(d.people, "person")}</div>
      <div class="add-row">
        <input id="add-person-input" type="text" placeholder="Add person…" />
        <button type="button" class="small" id="btn-add-person">+</button>
      </div>
    </div>
    <div class="row"><div class="label">Collections</div><div class="chips">${chips(d.collections, "collection")}</div>
      <div class="add-row">
        <select id="add-collection-select">
          <option value="">Add to collection…</option>
          ${state.collections
            .map((c) => {
              const already = d.collections.some((x) => x.id === c.id);
              return `<option value="${escapeAttr(c.id)}"${already ? " disabled" : ""}>${escapeHtml(c.name)}</option>`;
            })
            .join("")}
        </select>
        <button type="button" class="small" id="btn-add-collection">+</button>
      </div>
    </div>
    <div class="row"><div class="label">Tags</div><div class="chips">${chips(d.tags, "tag")}</div>
      <div class="add-row">
        <input id="add-tag-input" type="text" placeholder="Add tag…" />
        <button type="button" class="small" id="btn-add-tag">+</button>
      </div>
    </div>
    <div class="row"><div class="label">Original path</div><div class="value muted">${escapeHtml(a.original_path)}</div></div>
    <div class="row"><div class="label">Asset ID</div><div class="value muted">${escapeHtml(a.id)}</div></div>
    <div class="row"><div class="label">SHA-256</div><div class="value muted" style="font-size:11px">${escapeHtml(a.hash)}</div></div>
    ${
      d.parse_errors?.length
        ? `<div class="row"><div class="label">Parse notes</div><div class="value" style="color:var(--danger)">${d.parse_errors.map((e) => escapeHtml(e)).join("<br/>")}</div></div>`
        : ""
    }
  `;

  $("btn-add-tag")?.addEventListener("click", async () => {
    const input = $("add-tag-input");
    const name = input.value.trim();
    if (!name) return;
    await invoke("add_tag", { assetId: a.id, tag: name });
    input.value = "";
    await selectAsset(a.id);
    await refreshSidebar();
  });
  $("add-tag-input")?.addEventListener("keydown", async (e) => {
    if (e.key === "Enter") $("btn-add-tag")?.click();
  });

  $("btn-add-person")?.addEventListener("click", async () => {
    const input = $("add-person-input");
    const name = input.value.trim();
    if (!name) return;
    await invoke("add_person", { assetId: a.id, name });
    input.value = "";
    await selectAsset(a.id);
    await refreshSidebar();
  });
  $("add-person-input")?.addEventListener("keydown", async (e) => {
    if (e.key === "Enter") $("btn-add-person")?.click();
  });

  $("btn-add-collection")?.addEventListener("click", async () => {
    const sel = $("add-collection-select");
    const cid = sel.value;
    if (!cid) return;
    await invoke("add_to_collection", { assetId: a.id, collectionId: cid });
    sel.value = "";
    await selectAsset(a.id);
    await refreshSidebar();
  });

  $("detail-panel").querySelectorAll(".chip-x").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const { kind, id } = btn.dataset;
      try {
        if (kind === "tag") {
          await invoke("remove_tag", { assetId: a.id, tagId: id });
        } else if (kind === "person") {
          await invoke("remove_person", { assetId: a.id, personId: id });
        } else if (kind === "collection") {
          await invoke("remove_from_collection", {
            assetId: a.id,
            collectionId: id,
          });
        }
        await selectAsset(a.id);
        await refreshSidebar();
      } catch (err) {
        console.error(err);
      }
    });
  });

  setupPlayer(a);
}

// ── Audio player + waveform ─────────────────────────────────────────────────

function stopPlayer() {
  const p = state.player;
  if (p.raf) cancelAnimationFrame(p.raf);
  p.raf = 0;
  if (p.audio) {
    try {
      p.audio.pause();
      p.audio.removeAttribute("src");
      p.audio.load();
    } catch (_) {}
  }
  p.audio = null;
  if (p.revoke) {
    try {
      p.revoke();
    } catch (_) {}
  }
  p.revoke = null;
  p.peaks = null;
}

function fmtClock(sec) {
  if (!Number.isFinite(sec) || sec < 0) return "0:00";
  const s = Math.floor(sec);
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}:${String(r).padStart(2, "0")}`;
}

async function resolveAudioSrc(id) {
  // Preferred: stream the library file via convertFileSrc (asset protocol).
  try {
    const path = await invoke("asset_file_path", { id });
    const conv = window.__TAURI__?.core?.convertFileSrc;
    if (typeof conv === "function") {
      const src = conv(path);
      return { src, revoke: null };
    }
  } catch (e) {
    log("asset_file_path failed, falling back to bytes", e);
  }
  // Fallback: load bytes and build a blob URL.
  const resp = await invoke("asset_audio_bytes", { id });
  let blob;
  if (resp && typeof resp.blob === "function") {
    blob = await resp.blob();
  } else if (resp instanceof ArrayBuffer) {
    blob = new Blob([resp]);
  } else if (ArrayBuffer.isView(resp)) {
    blob = new Blob([resp.buffer.slice(resp.byteOffset, resp.byteOffset + resp.byteLength)]);
  } else if (Array.isArray(resp)) {
    blob = new Blob([new Uint8Array(resp)]);
  } else if (typeof resp === "string") {
    // base64
    const bin = atob(resp);
    const arr = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);
    blob = new Blob([arr]);
  } else {
    throw new Error("unsupported audio payload");
  }
  const src = URL.createObjectURL(blob);
  return { src, revoke: () => URL.revokeObjectURL(src) };
}

function drawWaveform(canvas, peaks, playedRatio) {
  const dpr = window.devicePixelRatio || 1;
  const cssW = canvas.clientWidth || 300;
  const cssH = canvas.clientHeight || 64;
  canvas.width = Math.max(1, Math.floor(cssW * dpr));
  canvas.height = Math.max(1, Math.floor(cssH * dpr));
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, cssW, cssH);

  const mid = cssH / 2;
  const n = peaks?.length || 0;
  if (!n) {
    ctx.strokeStyle = "rgba(139,151,168,0.35)";
    ctx.beginPath();
    ctx.moveTo(0, mid);
    ctx.lineTo(cssW, mid);
    ctx.stroke();
    return;
  }

  const barW = cssW / n;
  const playedX = playedRatio * cssW;
  for (let i = 0; i < n; i++) {
    const amp = Math.max(0.02, Math.min(1, peaks[i] || 0));
    const h = amp * (cssH * 0.9);
    const x = i * barW;
    const played = x < playedX;
    ctx.fillStyle = played ? "#5a9dff" : "#3d8bfd";
    ctx.globalAlpha = played ? 0.95 : 0.55;
    ctx.fillRect(x, mid - h / 2, Math.max(1, barW * 0.85), h);
  }
  ctx.globalAlpha = 1;
}

async function loadPeaks(id, buckets) {
  try {
    const r = await invoke("asset_peaks", { id, buckets });
    if (r?.peaks?.length) return r.peaks;
  } catch (e) {
    log("asset_peaks failed", e);
  }
  return null;
}

async function peaksViaWebAudio(src, assetId) {
  try {
    const AC = window.AudioContext || window.webkitAudioContext;
    if (!AC) return null;
    const resp = await fetch(src);
    const buf = await resp.arrayBuffer();
    const ac = new AC();
    const audioBuf = await ac.decodeAudioData(buf);
    const ch = audioBuf.getChannelData(0);
    const buckets = 480;
    const peaks = new Array(buckets).fill(0);
    const per = Math.max(1, Math.floor(ch.length / buckets));
    for (let b = 0; b < buckets; b++) {
      let peak = 0;
      const start = b * per;
      const end = Math.min(ch.length, start + per);
      for (let i = start; i < end; i += 8) {
        const v = Math.abs(ch[i]);
        if (v > peak) peak = v;
      }
      peaks[b] = peak;
    }
    ac.close?.();
    try {
      await invoke("save_asset_peaks", { id: assetId, peaks });
    } catch (e) {
      log("save_asset_peaks failed", e);
    }
    return peaks;
  } catch (e) {
    log("web audio peaks failed", e);
    return null;
  }
}

async function setupPlayer(asset) {
  stopPlayer();
  const wrap = $("waveform-wrap");
  const canvas = $("waveform");
  const btn = $("btn-play");
  const timeEl = $("player-time");
  const note = $("player-note");
  const playedEl = $("waveform-played");
  if (!canvas || !btn) return;

  const buckets = 480;
  state.player.peaks = await loadPeaks(asset.id, buckets);
  if (state.selectedId !== asset.id) return;
  drawWaveform(canvas, state.player.peaks, 0);

  let srcInfo;
  try {
    srcInfo = await resolveAudioSrc(asset.id);
  } catch (e) {
    log("resolveAudioSrc failed", e);
    if (note) note.textContent = "Audio unavailable";
    return;
  }
  if (state.selectedId !== asset.id) {
    srcInfo.revoke?.();
    return;
  }

  const audio = new Audio();
  audio.preload = "metadata";
  audio.src = srcInfo.src;
  state.player.audio = audio;
  state.player.revoke = srcInfo.revoke;

  const updateTime = () => {
    const dur = Number.isFinite(audio.duration) ? audio.duration : 0;
    const cur = audio.currentTime || 0;
    if (timeEl) timeEl.textContent = `${fmtClock(cur)} / ${fmtClock(dur)}`;
    const ratio = dur > 0 ? cur / dur : 0;
    if (playedEl) playedEl.style.width = `${Math.min(100, ratio * 100)}%`;
    drawWaveform(canvas, state.player.peaks, ratio);
  };

  audio.addEventListener("loadedmetadata", () => {
    updateTime();
    if (note) note.textContent = "";
  });
  audio.addEventListener("timeupdate", updateTime);
  audio.addEventListener("ended", () => {
    if (btn) btn.textContent = "▶";
    updateTime();
  });
  audio.addEventListener("error", () => {
    if (note) note.textContent = "Cannot play this file";
    if (btn) btn.textContent = "▶";
  });

  btn.addEventListener("click", async () => {
    if (!state.player.audio) return;
    if (state.player.audio.paused) {
      try {
        await state.player.audio.play();
        btn.textContent = "❚❚";
      } catch (e) {
        log("play failed", e);
        if (note) note.textContent = "Playback failed";
      }
    } else {
      state.player.audio.pause();
      btn.textContent = "▶";
    }
  });

  wrap.addEventListener("click", (e) => {
    const audioNow = state.player.audio;
    if (!audioNow || !Number.isFinite(audioNow.duration) || audioNow.duration <= 0) return;
    const rect = wrap.getBoundingClientRect();
    const ratio = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
    audioNow.currentTime = ratio * audioNow.duration;
    updateTime();
  });

  // Compressed formats: core cannot decode peaks — try Web Audio once.
  if (!state.player.peaks) {
    if (note) note.textContent = "Analyzing waveform…";
    const peaks = await peaksViaWebAudio(srcInfo.src, asset.id);
    if (state.selectedId !== asset.id) return;
    state.player.peaks = peaks;
    if (note) note.textContent = peaks ? "" : "Waveform unavailable";
    drawWaveform(canvas, state.player.peaks, 0);
  }
}

// ── Search ─────────────────────────────────────────────────────────────────

async function doSearch() {
  await refresh();
}

$("btn-search").addEventListener("click", doSearch);
$("search-input").addEventListener("keydown", (e) => {
  if (e.key === "Enter") doSearch();
});

// ── Import ─────────────────────────────────────────────────────────────────

$("btn-import").addEventListener("click", async () => {
  $("import-modal").classList.remove("hidden");
  $("scan-result").classList.add("hidden");
  $("btn-start-import").classList.add("hidden");
  $("import-progress").classList.add("hidden");
  state.pendingPaths = [];
  try {
    const pong = await invoke("debug_ping");
    log("debug_ping", pong);
    setMsg("import-status", `Ready (${pong}). Choose files, a folder, or type a path.`);
  } catch (e) {
    log("debug_ping failed", e);
    setMsg("import-status", `Backend ping failed: ${e}`, "error");
  }
});

$("btn-close-import").addEventListener("click", () => {
  $("import-modal").classList.add("hidden");
});

$("btn-select-files").addEventListener("click", async () => {
  setMsg("import-status", "Opening file picker…");
  try {
    const files = await pickFiles();
    log("picked files", files);
    if (!files || !files.length) {
      setMsg("import-status", "No files selected (or picker failed — use the path box below).");
      return;
    }
    state.pendingPaths = Array.isArray(files) ? files : [files];
    await previewScan();
  } catch (e) {
    log("select-files error", e);
    setMsg("import-status", `File picker error: ${e}`, "error");
  }
});

$("btn-select-folder").addEventListener("click", async () => {
  setMsg("import-status", "Opening folder picker…");
  try {
    const folder = await pickFolder();
    log("picked folder", folder);
    if (!folder) {
      setMsg("import-status", "No folder selected (or picker failed — use the path box below).");
      return;
    }
    state.pendingPaths = [folder];
    await previewScan();
  } catch (e) {
    log("select-folder error", e);
    setMsg("import-status", `Folder picker error: ${e}`, "error");
  }
});

// Manual path fallback — works even if native picker is broken.
const btnAddPath = $("btn-add-path");
if (btnAddPath) {
  btnAddPath.addEventListener("click", async () => {
    const input = $("manual-path");
    const p = (input?.value || "").trim();
    if (!p) {
      setMsg("import-status", "Type a file or folder path first.");
      return;
    }
    state.pendingPaths = [p];
    await previewScan();
  });
}

// Drag & drop
const drop = $("import-drop");
if (drop) {
  drop.addEventListener("dragover", (e) => {
    e.preventDefault();
    drop.style.borderColor = "var(--accent)";
  });
  drop.addEventListener("dragleave", () => {
    drop.style.borderColor = "";
  });
  drop.addEventListener("drop", async (e) => {
    e.preventDefault();
    drop.style.borderColor = "";
    // Tauri v2 may expose file paths via event or webview drag-drop plugin.
    // MVP: fall back to dialog picker if web paths are not available.
    const files = e.dataTransfer?.files;
    if (files?.length && window.__TAURI__.webviewWindow) {
      // Best-effort: some builds expose paths on File objects
      const paths = Array.from(files)
        .map((f) => f.path || f.name)
        .filter(Boolean);
      if (paths.length) {
        state.pendingPaths = paths;
        await previewScan();
        return;
      }
    }
    const folder = await pickFolder();
    if (folder) {
      state.pendingPaths = [folder];
      await previewScan();
    }
  });
}

async function previewScan() {
  if (!state.pendingPaths.length) {
    setMsg("import-status", "No paths to scan.");
    return;
  }
  const el = $("scan-result");
  const btn = $("btn-start-import");
  if (el) {
    el.classList.remove("hidden");
    el.textContent = `Scanning ${state.pendingPaths.length} path(s)… hashing may take a while.`;
  }
  if (btn) btn.classList.add("hidden");
  setMsg("import-status", "Scanning…");
  log("scan_paths", state.pendingPaths);
  try {
    const scan = await invoke("scan_paths", { paths: state.pendingPaths });
    log("scan result", scan);
    el.classList.remove("hidden");
    el.innerHTML = `
      <strong>Found ${scan.ready + scan.duplicate + scan.unsupported + scan.error} files</strong>
      <div>${scan.ready} Ready · ${scan.duplicate} Duplicate · ${scan.unsupported} Unsupported · ${scan.error} Error</div>
      <div class="muted" style="margin-top:6px;max-height:120px;overflow:auto;font-size:11px">
        ${(scan.files || [])
          .slice(0, 20)
          .map((f) => `${escapeHtml(f.path)}${f.error ? " — " + escapeHtml(f.error) : ""}`)
          .join("<br/>")}
      </div>
    `;
    btn.classList.remove("hidden");
    setMsg("import-status", `Scan OK: ${scan.ready} ready.`);
  } catch (e) {
    log("scan failed", e);
    el.classList.remove("hidden");
    el.textContent = `Scan failed: ${e}`;
    setMsg("import-status", `Scan failed: ${e}`, "error");
  }
}

$("btn-start-import").addEventListener("click", async () => {
  if (!state.pendingPaths.length) return;
  const onDuplicate = $("duplicate-action").value;
  const progress = $("import-progress");
  progress.classList.remove("hidden");
  progress.textContent = "Importing…";
  $("btn-start-import").disabled = true;
  setMsg("import-status", "Importing…");
  log("import_paths", state.pendingPaths, onDuplicate);
  try {
    const r = await invoke("import_paths", {
      paths: state.pendingPaths,
      onDuplicate,
    });
    log("import result", r);
    progress.innerHTML = `
      <strong>Import complete</strong><br/>
      ${r.success} imported · ${r.duplicate} duplicate · ${r.unsupported} unsupported · ${r.failed} failed
    `;
    setMsg("import-status", `Imported ${r.success} file(s).`, "ok");
    await refresh();
  } catch (e) {
    log("import failed", e);
    progress.textContent = `Import failed: ${e}`;
    setMsg("import-status", `Import failed: ${e}`, "error");
  } finally {
    $("btn-start-import").disabled = false;
  }
});

// ── Recovery ───────────────────────────────────────────────────────────────

async function checkRecovery() {
  try {
    const report = await invoke("recovery_report");
    const jobs = await invoke("incomplete_jobs");
    const needs =
      (jobs && jobs.length > 0) ||
      (report &&
        (report.incomplete_assets > 0 || report.library_files > 0));
    if (needs) {
      $("btn-recovery").classList.remove("hidden");
      $("btn-recovery").textContent =
        jobs?.length > 0
          ? `Resume Incomplete (${jobs.length})`
          : "Resume Incomplete";
    }
  } catch (_) {
    try {
      const jobs = await invoke("incomplete_jobs");
      if (jobs?.length) $("btn-recovery").classList.remove("hidden");
    } catch (_) {
      /* ignore */
    }
  }
}

$("btn-recovery").addEventListener("click", async () => {
  const jobs = await invoke("incomplete_jobs");
  const list = $("recovery-list");
  list.innerHTML = jobs.length
    ? jobs
        .map(
          (j) => `
      <div class="job-row" data-job-row="${escapeAttr(j.id)}">
        <div>
          <div>${escapeHtml(j.source)}</div>
          <div class="muted">${escapeHtml(j.status)} · ${j.processed_files}/${j.total_files}</div>
        </div>
        <div class="job-actions">
          <button class="primary small" data-act="resume" data-job="${escapeAttr(j.id)}">Resume</button>
          <button class="small" data-act="retry" data-job="${escapeAttr(j.id)}">Retry</button>
          <button class="small danger" data-act="cleanup" data-job="${escapeAttr(j.id)}">Clean Up</button>
        </div>
      </div>`
        )
        .join("")
    : '<div class="muted">No unfinished imports.</div>';
  $("recovery-modal").classList.remove("hidden");

  list.querySelectorAll("button[data-job]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const act = btn.dataset.act;
      const jobId = btn.dataset.job;
      const row = btn.closest(".job-row");
      const all = row.querySelectorAll("button");
      all.forEach((b) => (b.disabled = true));

      if (act === "cleanup") {
        const ok = window.confirm(
          "Clean Up discards unfinished copies for this import. Ready assets are kept. Continue?"
        );
        if (!ok) {
          all.forEach((b) => (b.disabled = false));
          return;
        }
        btn.textContent = "Cleaning…";
        try {
          const removed = await invoke("cleanup_job", { jobId });
          btn.textContent = `Cleaned (${removed})`;
          row.querySelector(".muted").textContent = `cancelled · removed ${removed} partial`;
          await refresh();
          await checkRecovery();
        } catch (e) {
          btn.textContent = `Error: ${e}`;
          all.forEach((b) => (b.disabled = false));
        }
        return;
      }

      const label = act === "retry" ? "Retrying…" : "Resuming…";
      btn.textContent = label;
      try {
        const r = await invoke(act === "retry" ? "retry_job" : "resume_job", {
          jobId,
        });
        btn.textContent = `Done (${r.success} ok)`;
        await refresh();
        await checkRecovery();
      } catch (e) {
        btn.textContent = `Error: ${e}`;
        all.forEach((b) => (b.disabled = false));
      }
    });
  });
});

$("btn-close-recovery").addEventListener("click", () => {
  $("recovery-modal").classList.add("hidden");
});

// ── Quick actions ──────────────────────────────────────────────────────────

$("btn-new-collection").addEventListener("click", async () => {
  const name = window.prompt("Collection name:");
  if (!name) return;
  await invoke("create_collection", {
    name,
    assetId: state.selectedId || null,
  });
  if (state.selectedId) await selectAsset(state.selectedId);
});

// ── Smart Collections ──────────────────────────────────────────────────────

$("btn-new-smart")?.addEventListener("click", () => {
  $("smart-modal")?.classList.remove("hidden");
  $("smart-status").textContent = "";
});

$("btn-close-smart")?.addEventListener("click", () => {
  $("smart-modal")?.classList.add("hidden");
});

$("btn-save-smart")?.addEventListener("click", async () => {
  const name = $("smart-name")?.value?.trim();
  if (!name) {
    $("smart-status").textContent = "Name is required.";
    return;
  }
  const field = $("smart-field").value;
  const op = $("smart-op").value;
  let raw = $("smart-value").value.trim();
  if (field === "has_gps") raw = raw === "false" ? false : true;
  else if (field === "sample_rate") raw = Number(raw) || 0;

  const rules = {
    match: $("smart-match").value,
    conditions: [{ field, op, value: raw }],
  };
  try {
    await invoke("create_smart_collection", { name, rules });
    $("smart-modal").classList.add("hidden");
    await refreshSidebar();
    await refresh();
  } catch (e) {
    $("smart-status").textContent = String(e);
  }
});

// ── Batch edit ─────────────────────────────────────────────────────────────

$("btn-multi")?.addEventListener("click", () => {
  state.multiSelect = !state.multiSelect;
  state.selectedSet = new Set();
  $("btn-multi").classList.toggle("primary", state.multiSelect);
  renderTable(state.assets);
});

$("batch-clear")?.addEventListener("click", () => {
  state.selectedSet = new Set();
  renderTable(state.assets);
});

async function batchOp(fn) {
  const ids = [...(state.selectedSet || [])];
  if (!ids.length) return;
  try {
    await fn(ids);
    await refreshSidebar();
    await refresh();
  } catch (e) {
    log("batch op failed", e);
  }
}

$("batch-add-tag")?.addEventListener("click", () => {
  const tag = $("batch-tag").value.trim();
  if (!tag) return;
  batchOp((ids) => invoke("batch_add_tag", { assetIds: ids, tag }));
});

$("batch-remove-tag")?.addEventListener("click", () => {
  const tag = $("batch-tag").value.trim();
  if (!tag) return;
  batchOp((ids) => invoke("batch_remove_tag", { assetIds: ids, tag }));
});

$("batch-add-person")?.addEventListener("click", () => {
  const name = $("batch-person").value.trim();
  if (!name) return;
  batchOp((ids) => invoke("batch_add_person", { assetIds: ids, name }));
});

$("batch-add-col")?.addEventListener("click", () => {
  const cid = $("batch-collection").value;
  if (!cid) return;
  batchOp((ids) => invoke("batch_add_to_collection", { assetIds: ids, collectionId: cid }));
});

$("batch-remove-col")?.addEventListener("click", () => {
  const cid = $("batch-collection").value;
  if (!cid) return;
  batchOp((ids) => invoke("batch_remove_from_collection", { assetIds: ids, collectionId: cid }));
});

// ── Trash + Undo ───────────────────────────────────────────────────────────

async function showDuplicates() {
  const table = document.querySelector(".asset-table");
  const empty = $("empty-state");
  if (table) table.classList.add("hidden");
  if (empty) empty.classList.add("hidden");
  $("map-panel")?.classList.add("hidden");

  let groups = [];
  try {
    groups = await invoke("list_duplicate_groups");
  } catch (e) {
    log("list_duplicate_groups failed", e);
  }

  const panel = $("map-panel");
  if (!panel) return;
  panel.classList.remove("hidden");
  panel.innerHTML = "";
  const title = document.createElement("div");
  title.className = "section-label";
  title.textContent = groups.length
    ? `${groups.length} duplicate group(s) — same bytes, different entries`
    : "No in-library duplicates. Nice.";
  panel.appendChild(title);

  for (const g of groups) {
    const box = document.createElement("div");
    box.style.margin = "12px 0";
    box.style.padding = "10px";
    box.style.border = "1px solid #243044";
    box.style.borderRadius = "8px";
    const rows = g.assets
      .map(
        (a) =>
          `<div class="muted" style="font-size:12px">${escapeHtml(a.filename)} · ${escapeHtml(a.original_path)} · ${fmtSize(a.file_size)} · ${escapeHtml(fmtDate(a.imported_at))}</div>`
      )
      .join("");
    box.innerHTML = `
      <div><strong>${escapeHtml(g.assets[0].filename)}</strong> × ${g.assets.length}
        <span class="muted">· ${fmtSize(g.file_size)} · hash ${escapeHtml(g.hash.slice(0, 12))}…</span></div>
      ${rows}
      <div style="margin-top:8px">
        <button type="button" class="ghost small" data-hash="${escapeAttr(g.hash)}" data-act="oldest">Keep oldest</button>
        <button type="button" class="ghost small" data-hash="${escapeAttr(g.hash)}" data-act="newest">Keep newest</button>
      </div>`;
    panel.appendChild(box);
  }

  panel.querySelectorAll("button[data-hash]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      try {
        const n = await invoke("dedupe_library", {
          strategy: btn.dataset.act,
          hash: btn.dataset.hash,
        });
        log("deduped group", n);
        await showDuplicates();
        await refreshUndoButton();
      } catch (e) {
        window.alert(String(e));
      }
    });
  });

  const count = $("asset-count");
  if (count) count.textContent = `${groups.length} duplicate group(s)`;
}

$("btn-dedupe")?.addEventListener("click", async () => {
  const strategy = $("dedupe-strategy")?.value || "oldest";
  if (!window.confirm("Collapse ALL duplicate groups? Extra copies go to the recycle bin (undoable).")) return;
  try {
    const n = await invoke("dedupe_library", { strategy, hash: null });
    log("deduped", n);
    await showDuplicates();
    await refreshUndoButton();
  } catch (e) {
    window.alert(String(e));
  }
});

// ── Playlists ──────────────────────────────────────────────────────────────

async function showPlaylists() {
  const table = document.querySelector(".asset-table");
  const empty = $("empty-state");
  if (table) table.classList.add("hidden");
  if (empty) empty.classList.add("hidden");
  $("map-panel")?.classList.add("hidden");

  let playlists = [];
  try {
    playlists = await invoke("list_playlists");
  } catch (e) {
    log("list_playlists failed", e);
  }
  const sel = $("playlist-select");
  if (sel) {
    const cur = sel.value;
    sel.innerHTML =
      '<option value="">Select playlist…</option>' +
      playlists
        .map(
          (p) =>
            `<option value="${escapeAttr(p.id)}">${escapeHtml(p.name)} (${p.track_count})</option>`
        )
        .join("");
    if (cur) sel.value = cur;
    if (!sel.value && playlists[0]) sel.value = playlists[0].id;
  }

  const pid = $("playlist-select")?.value;
  const panel = $("map-panel");
  if (!panel) return;
  panel.classList.remove("hidden");
  panel.innerHTML = "";

  const title = document.createElement("div");
  title.className = "section-label";
  title.textContent = pid ? "Playlist tracks (use ↑ ↓ to reorder)" : "Create or select a playlist";
  panel.appendChild(title);

  if (!pid) return;

  let tracks = [];
  try {
    tracks = await invoke("list_playlist_tracks", { playlistId: pid });
  } catch (e) {
    log("list_playlist_tracks failed", e);
  }

  const list = document.createElement("div");
  tracks.forEach((t, i) => {
    const row = document.createElement("div");
    row.style.cssText =
      "display:flex;align-items:center;gap:6px;padding:6px;border-bottom:1px solid #1a2740";
    row.innerHTML = `
      <span class="muted" style="width:24px">${i + 1}</span>
      <span style="flex:1">${escapeHtml(t.filename)}</span>
      <button type="button" class="ghost small" data-act="up" data-idx="${i}">↑</button>
      <button type="button" class="ghost small" data-act="down" data-idx="${i}">↓</button>
      <button type="button" class="ghost small" data-act="rm" data-idx="${i}">✕</button>`;
    list.appendChild(row);
  });
  panel.appendChild(list);

  list.querySelectorAll("button[data-act]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const i = Number(btn.dataset.idx);
      const act = btn.dataset.act;
      try {
        if (act === "up" && i > 0) {
          await invoke("playlist_move_track", { playlistId: pid, from: i, to: i - 1 });
        } else if (act === "down" && i < tracks.length - 1) {
          await invoke("playlist_move_track", { playlistId: pid, from: i, to: i + 1 });
        } else if (act === "rm") {
          await invoke("playlist_remove_track", {
            playlistId: pid,
            assetId: tracks[i].asset_id,
          });
        }
        await showPlaylists();
      } catch (e) {
        window.alert(String(e));
      }
    });
  });
}

$("playlist-select")?.addEventListener("change", () => showPlaylists());

$("btn-new-playlist")?.addEventListener("click", async () => {
  const name = window.prompt("Playlist name:");
  if (!name) return;
  try {
    await invoke("create_playlist", { name });
    await showPlaylists();
  } catch (e) {
    window.alert(String(e));
  }
});

$("pl-add-selected")?.addEventListener("click", async () => {
  const pid = $("playlist-select")?.value;
  if (!pid) {
    window.alert("Select a playlist first.");
    return;
  }
  const ids = selectedIds();
  if (!ids.length) {
    window.alert("Select assets in the All Sounds view first (Multi-select).");
    return;
  }
  try {
    await invoke("playlist_add_tracks", { playlistId: pid, assetIds: ids });
    await showPlaylists();
  } catch (e) {
    window.alert(String(e));
  }
});

function selectedIds() {
  if (state.selectedSet && state.selectedSet.size) return [...state.selectedSet];
  if (state.selectedId) return [state.selectedId];
  return [];
}

$("btn-delete")?.addEventListener("click", async () => {
  const ids = selectedIds();
  if (!ids.length) {
    window.alert("Select one or more assets first (use Multi-select).");
    return;
  }
  if (!window.confirm(`Move ${ids.length} item(s) to the recycle bin?`)) return;
  try {
    await invoke("soft_delete_assets", { assetIds: ids });
    state.selectedSet = new Set();
    state.selectedId = null;
    await refresh();
  } catch (e) {
    log("soft_delete failed", e);
    window.alert(String(e));
  }
});

$("btn-undo")?.addEventListener("click", async () => {
  try {
    const label = await invoke("undo_last");
    log("undid", label);
    await refresh();
    if (state.selectedId) await selectAsset(state.selectedId);
  } catch (e) {
    log("undo failed", e);
    window.alert(String(e));
  }
});

$("trash-restore")?.addEventListener("click", async () => {
  const ids = selectedIds();
  if (!ids.length) return;
  try {
    await invoke("restore_assets", { assetIds: ids });
    state.selectedSet = new Set();
    await refresh();
  } catch (e) {
    log("restore failed", e);
  }
});

$("trash-purge")?.addEventListener("click", async () => {
  const ids = selectedIds();
  if (!ids.length) return;
  if (!window.confirm(`Permanently delete ${ids.length} item(s)? This cannot be undone.`)) return;
  try {
    await invoke("purge_assets", { assetIds: ids });
    state.selectedSet = new Set();
    await refresh();
  } catch (e) {
    log("purge failed", e);
  }
});

$("btn-empty-trash")?.addEventListener("click", async () => {
  if (!window.confirm("Permanently delete ALL items in the recycle bin?")) return;
  try {
    const n = await invoke("empty_trash");
    log("emptied trash", n);
    await refresh();
  } catch (e) {
    log("empty_trash failed", e);
  }
});

async function refreshUndoButton() {
  const btn = $("btn-undo");
  if (!btn) return;
  try {
    const label = await invoke("undo_peek");
    btn.title = label ? `Undo: ${label}` : "Nothing to undo";
    btn.disabled = !label;
  } catch (_) {
    btn.disabled = false;
  }
}

// ── Tag manager ────────────────────────────────────────────────────────────

$("tag-match-mode")?.addEventListener("change", (e) => {
  state.filter.tagsAll = e.target.value === "all";
  refresh();
});

$("btn-tag-manager")?.addEventListener("click", () => {
  $("tag-manager")?.classList.toggle("hidden");
  renderTagManager();
});

function renderTagManager() {
  const box = $("tag-manager");
  if (!box || box.classList.contains("hidden")) return;
  const rows = state.tags
    .map((t) => {
      const n = t.asset_count != null ? ` (${t.asset_count})` : "";
      return `<div style="display:flex;align-items:center;gap:4px;margin:2px 0">
        <span style="flex:1">${escapeHtml(t.name)}${n}</span>
        <button type="button" class="ghost small" data-act="rename" data-id="${escapeAttr(t.id)}" data-name="${escapeAttr(t.name)}">✎</button>
        <button type="button" class="ghost small" data-act="merge" data-id="${escapeAttr(t.id)}" data-name="${escapeAttr(t.name)}">⇥</button>
        <button type="button" class="ghost small" data-act="delete" data-id="${escapeAttr(t.id)}" data-name="${escapeAttr(t.name)}">✕</button>
      </div>`;
    })
    .join("");
  box.innerHTML = rows || '<span class="muted">No tags</span>';

  box.querySelectorAll("button[data-act]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const id = btn.dataset.id;
      const name = btn.dataset.name;
      const act = btn.dataset.act;
      try {
        if (act === "rename") {
          const next = window.prompt(`Rename “${name}” to:`, name);
          if (!next || next === name) return;
          await invoke("rename_tag", { tagId: id, newName: next });
        } else if (act === "delete") {
          if (!window.confirm(`Delete tag “${name}” from all assets? (undoable)`)) return;
          await invoke("delete_tag", { tagId: id });
        } else if (act === "merge") {
          const others = state.tags.filter((t) => t.id !== id);
          const list = others.map((t) => `${t.name}`).join(", ");
          const into = window.prompt(`Merge “${name}” INTO which tag?\nAvailable: ${list}`);
          if (!into) return;
          const target = others.find((t) => t.name === into);
          if (!target) {
            window.alert("Target tag not found.");
            return;
          }
          await invoke("merge_tags", { fromId: id, toId: target.id });
        }
        await refreshSidebar();
        await refresh();
        await refreshUndoButton();
      } catch (e) {
        window.alert(String(e));
      }
    });
  });
}

function fillBatchCollections() {
  const sel = $("batch-collection");
  if (!sel) return;
  const cur = sel.value;
  sel.innerHTML =
    '<option value="">Collection…</option>' +
    state.collections
      .filter((c) => c.collection_type !== "smart")
      .map((c) => `<option value="${escapeAttr(c.id)}">${escapeHtml(c.name)}</option>`)
      .join("");
  if (cur) sel.value = cur;
}

document.querySelectorAll(".nav-item").forEach((el) => {
  el.addEventListener("click", async () => {
    document.querySelectorAll(".nav-item").forEach((n) => n.classList.remove("active"));
    el.classList.add("active");
    state.currentView = el.dataset.view;
    state.filter.tag = null;
    state.filter.tags = [];
    state.filter.collectionId = null;
    if ($("search-input")) $("search-input").value = "";
    if (state.currentView === "map") {
      await showMap();
      bindMapClick();
    } else {
      $("map-panel")?.classList.add("hidden");
      document.querySelector(".asset-table")?.classList.remove("hidden");
      await refresh();
    }
    await refreshUndoButton();
  });
});

// Boot
tryRestore();
