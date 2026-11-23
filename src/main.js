const invoke = window.__TAURI__?.core?.invoke;
let serverBase = "";
const ACTIVE = [
  "initializing",
  "uploading",
  "completing",
  "assembling",
  "uploaded",
  "server_processing",
  "converting",
  "processing",
  "moving",
  "restarting",
];

const RANK = {
  initializing: 0,
  uploading: 1,
  completing: 2,
  assembling: 2,
  uploaded: 3,
  server_processing: 4,
  converting: 4,
  processing: 4,
  moving: 4,
  restarting: 4,
  pending: 5,
  missing_file: 6,
  failed: 7,
  done: 8,
};

const LABEL = {
  pending: "queued",
  initializing: "starting",
  uploading: "uploading",
  completing: "assembling",
  assembling: "assembling",
  uploaded: "uploaded",
  server_processing: "converting",
  converting: "converting",
  processing: "converting",
  moving: "converting",
  restarting: "converting",
  done: "uploaded",
  failed: "error",
  missing_file: "file missing",
};

const frames = new Map();
const samples = new Map();
const speeds = new Map();

function $(id) {
  return document.getElementById(id);
}

function setError(text) {
  const message = text || "";
  $("form-error").textContent = message;
  $("banner").textContent = message;
}

async function call(name, args) {
  try {
    setError("");
    return await invoke(name, args);
  } catch (err) {
    const text = typeof err === "string" ? err : err?.message || String(err);
    setError(text);
    throw err;
  }
}

function formatBytes(bytes) {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  const digits = index === 0 ? 0 : value >= 100 ? 0 : 1;
  return `${value.toFixed(digits)} ${units[index]}`;
}

function percent(job) {
  if (job.status === "done") return 100;
  if (job.status === "failed") return 0;
  if (job.status === "uploading") {
    if (!job.totalChunks) return 0;
    return Math.min(100, Math.round((job.uploadedChunks / job.totalChunks) * 100));
  }
  if (ACTIVE.includes(job.status)) {
    return Math.min(100, job.serverProgress || 0);
  }
  return 0;
}

function bytesSent(job) {
  if (!job.size || !job.totalChunks) return 0;
  return Math.min(job.size, (job.size * job.uploadedChunks) / job.totalChunks);
}

function speedOf(job) {
  if (job.status !== "uploading") return null;
  const sent = bytesSent(job);
  const now = Date.now();
  const prev = samples.get(job.id);
  const last = speeds.get(job.id) || null;
  if (!prev) {
    samples.set(job.id, { t: now, sent });
    return last;
  }
  if (now - prev.t < 1000) return last;
  samples.set(job.id, { t: now, sent });
  if (sent <= prev.sent) return last;
  const bps = (sent - prev.sent) / ((now - prev.t) / 1000);
  if (bps < 1) return last;
  speeds.set(job.id, { bps });
  return { bps };
}

function apiRoot() {
  return serverBase.trim().replace(/\/$/, "");
}

function videoLink(job) {
  const base = apiRoot();
  if (!base) return null;
  if (job.slug) return `${base}/v/${encodeURIComponent(job.slug)}`;
  if (job.videoId) return `${base}/watch/${encodeURIComponent(job.videoId)}`;
  return null;
}

function thumb(job) {
  const icons = [
    "M8 34 L24 8 L40 34 M16 26 H32",
    "M24 6 A18 18 0 1 0 24.1 6 M24 14 V24 L32 28",
    "M8 30 L18 12 L28 30 L38 12",
    "M10 24 H38 M24 10 V38 M14 14 L34 34 M34 14 L14 34",
    "M24 8 L30 20 H40 L32 28 L35 40 L24 33 L13 40 L16 28 L8 20 H18 Z",
  ];
  const name = job.filename || "";
  let hash = 0;
  for (const ch of name) hash = (hash * 33 + ch.charCodeAt(0)) >>> 0;
  const icon = icons[hash % icons.length];
  const wrap = document.createElement("div");
  wrap.className = "thumb";
  const paintIcon = () => {
    wrap.innerHTML = `<svg viewBox="0 0 48 48" fill="none" stroke="#ff7adf" stroke-width="2.2" stroke-linejoin="round"><path d="${icon}"/></svg>`;
  };
  const paint = (src) => {
    const img = document.createElement("img");
    img.alt = "";
    img.src = src;
    wrap.replaceChildren(img);
  };
  const hit = job.path ? frames.get(job.path) : null;
  if (hit?.src && hit.size === job.size) {
    paint(hit.src);
    return wrap;
  }
  paintIcon();
  if (!job.path || !invoke) return wrap;
  if (
    job.status === "pending" ||
    job.status === "initializing" ||
    job.status === "uploading" ||
    ACTIVE.includes(job.status)
  ) {
    return wrap;
  }
  if (hit?.pending) return wrap;
  const same = hit && hit.size === job.size;
  const misses = same ? hit.misses || 0 : 0;
  if (same && !hit.src && (misses >= 3 || Date.now() - hit.at < 20000)) return wrap;
  frames.set(job.path, { src: null, size: job.size, at: Date.now(), pending: true, misses });
  invoke("thumbnail", { path: job.path })
    .then((src) => {
      frames.set(job.path, {
        src: src || null,
        size: job.size,
        at: Date.now(),
        pending: false,
        misses: src ? 0 : misses + 1,
      });
      if (src && wrap.isConnected) paint(src);
    })
    .catch(() => {
      frames.set(job.path, { src: null, size: job.size, at: Date.now(), pending: false, misses: misses + 1 });
    });
  return wrap;
}

function detailLine(job) {
  if (job.status === "done") {
    return { text: "Uploaded · available on the website", ok: true };
  }
  if (job.status === "missing_file") {
    const err = job.error || "Local file not found";
    return { text: `interrupted · ${err}`, error: true };
  }
  if (job.status === "failed") {
    const err = job.error || "Upload failed";
    return { text: `error · ${err}`, error: true };
  }
  const tag = LABEL[job.status] || job.status;
  const speed = speedOf(job);
  if (job.status === "uploading" && speed) {
    return { text: `${tag} · ${formatBytes(speed.bps)}/s` };
  }
  if (job.status === "pending") {
    return { text: `${tag} · waiting` };
  }
  return { text: tag };
}

function renderAccount(auth, config) {
  const signedIn = Boolean(auth.loggedIn);
  $("gate").hidden = signedIn;
  $("account-user").hidden = !signedIn;
  $("account-note").textContent = signedIn ? "Connected" : "Same account as on the server";
  if (signedIn) {
    $("account-name").textContent = auth.username || "—";
  } else {
    if (document.activeElement !== $("username") && auth.username) {
      $("username").value = auth.username;
    }
    const savedUrl = config?.serverUrl?.trim() || "";
    if (document.activeElement !== $("server-url") && savedUrl) {
      $("server-url").value = savedUrl;
    }
  }
}

function renderFolder(snap) {
  const folder = snap.config?.watchFolder?.trim() || "";
  const loggedIn = Boolean(snap.auth?.loggedIn);
  $("watch-path").textContent = folder || "No folder selected";
  $("watch-path").title = folder || "Choose a replay folder";
  const flag = $("watch-status");
  if (!flag) return;
  if (!loggedIn) {
    flag.textContent = "SIGN IN";
    flag.classList.add("off");
  } else if (!folder) {
    flag.textContent = "NO FOLDER";
    flag.classList.add("off");
  } else {
    flag.textContent = "WATCHING";
    flag.classList.remove("off");
  }
}

function renderQueue(jobs) {
  const queue = $("queue");
  queue.replaceChildren();
  jobs = jobs
    .filter((j) => j.status !== "dismissed")
    .sort((a, b) => (RANK[a.status] ?? 9) - (RANK[b.status] ?? 9) || b.id - a.id);

  const active = jobs.filter((j) => ACTIVE.includes(j.status)).length;
  const queued = jobs.filter((j) => j.status === "pending").length;
  $("upload-count").textContent = `${active} Active · ${queued} Queued`;

  if (jobs.length === 0) {
    const empty = document.createElement("p");
    empty.className = "empty";
    empty.textContent = "No uploads yet. Choose a folder or upload a file.";
    queue.append(empty);
    return;
  }

  for (const job of jobs) {
    const row = document.createElement("article");
    row.className = "job";
    const body = document.createElement("div");
    body.className = "body";

    const name = document.createElement("p");
    name.className = "name";
    name.textContent = job.filename;
    name.title = job.path;

    const meta = document.createElement("p");
    meta.className = "meta";
    meta.textContent = formatBytes(job.size);

    const pct = percent(job);
    const progress = document.createElement("div");
    progress.className = "progress-wrap";
    const track = document.createElement("div");
    track.className = "track";
    const fill = document.createElement("div");
    fill.className = "fill";
    if (job.status === "failed" || job.status === "missing_file") fill.classList.add("error");
    else if (job.status === "done") fill.classList.add("ready");
    else if (job.status === "pending") fill.classList.add("queued");
    else if (
      job.status === "converting" ||
      job.status === "processing" ||
      job.status === "server_processing"
    ) {
      fill.classList.add("converting");
    } else if (job.status === "assembling" || job.status === "completing") {
      fill.classList.add("assembling");
    } else {
      fill.classList.add("uploading");
    }
    fill.style.width = `${pct}%`;
    track.append(fill);
    const pctNode = document.createElement("span");
    pctNode.className = "pct";
    pctNode.textContent = `${pct}%`;
    progress.append(track, pctNode);

    const line = detailLine(job);
    const detail = document.createElement("p");
    detail.className = "detail";
    if (line.error) detail.classList.add("error");
    if (line.ok) detail.classList.add("ok");
    detail.textContent = line.text;

    body.append(name, meta, progress, detail);

    if (job.status === "failed") {
      const actions = document.createElement("div");
      actions.className = "job-actions";
      const retry = document.createElement("button");
      retry.type = "button";
      retry.textContent = "RETRY";
      retry.addEventListener("click", () => call("retry_job", { id: job.id }).then(refresh));
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "muted";
      remove.textContent = "REMOVE";
      remove.addEventListener("click", () => call("dismiss_job", { id: job.id }).then(refresh));
      actions.append(retry, remove);
      body.append(actions);
    } else if (job.status === "done") {
      const link = videoLink(job);
      if (link) {
        row.style.cursor = "pointer";
        row.title = "Open on the website";
        row.addEventListener("click", () => call("open_external", { url: link }));
      }
    }

    row.append(thumb(job), body);
    queue.append(row);
  }
}

function render(snap) {
  const jobs = snap.jobs || [];
  serverBase = snap.config?.serverUrl || "";
  renderAccount(snap.auth || {}, snap.config || {});
  renderFolder(snap);
  renderQueue(jobs);
}

async function refresh() {
  if (!invoke) return;
  try {
    render(await invoke("snapshot"));
  } catch (err) {
    setError(String(err));
  }
}

function openDialog(id) {
  $("form-error").textContent = "";
  $(id).showModal();
  if (id === "help-dialog") refreshAutostart();
}

async function refreshAutostart() {
  const box = $("autostart");
  if (!box || !invoke) return;
  try {
    box.checked = Boolean(await invoke("plugin:autostart|is_enabled"));
  } catch (err) {
    setError(typeof err === "string" ? err : err?.message || String(err));
  }
}

function currentWindow() {
  return window.__TAURI__?.window?.getCurrentWindow?.();
}

function on(id, action) {
  $(id).addEventListener("click", () => {
    Promise.resolve(action()).catch(() => {});
  });
}

function init() {
  if (!invoke) {
    setError("Open with npm run dev, not as a plain file.");
    return;
  }

  document.querySelector(".top")?.addEventListener("mousedown", (event) => {
    if (event.button !== 0 || event.target.closest("button")) return;
    currentWindow()?.startDragging();
  });

  on("dot-min", () => currentWindow()?.minimize());
  on("dot-max", () => currentWindow()?.toggleMaximize());
  on("dot-close", () => currentWindow()?.close());

  on("watch-change", () => call("pick_watch_folder"));
  on("upload-now-btn", () => call("upload_file_now"));
  on("logout-btn", () => call("logout").then(refresh));
  on("privacy-btn", () => openDialog("privacy-dialog"));
  on("help-btn", () => openDialog("help-dialog"));
  on("website-btn", () => {
    const url = apiRoot();
    if (!url) {
      setError("Server URL is not configured");
      return;
    }
    call("open_external", { url });
  });

  $("gate").addEventListener("submit", (event) => {
    event.preventDefault();
    call("login", {
      username: $("username").value,
      password: $("password").value,
      serverUrl: $("server-url").value,
    })
      .then(() => {
        $("password").value = "";
        return refresh();
      })
      .catch(() => {});
  });

  $("autostart").addEventListener("change", () => {
    const enabled = $("autostart").checked;
    invoke(enabled ? "plugin:autostart|enable" : "plugin:autostart|disable").catch((err) => {
      $("autostart").checked = !enabled;
      setError(typeof err === "string" ? err : err?.message || String(err));
    });
  });

  refreshAutostart();

  const listen = window.__TAURI__?.event?.listen;
  if (listen) listen("snapshot", (event) => render(event.payload));
  refresh();
  setInterval(refresh, 4000);
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}