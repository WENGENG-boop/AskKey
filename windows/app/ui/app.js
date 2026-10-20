// Ask Key management window. Every piece of user or agent data is inserted
// with textContent, never as HTML.
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const language = (navigator.language || "en").toLowerCase().startsWith("zh") ? "zh" : "en";
document.documentElement.lang = language === "zh" ? "zh-CN" : "en";
const strings = window.ASKKEY_STRINGS[language];
const t = (key) => strings[key] ?? window.ASKKEY_STRINGS.en[key] ?? key;

const state = {
  tab: "requests",
  info: null,
  unlocked: false,
  credentials: [],
  pending: [],
  records: [],
  clients: [],
};

// ---------- DOM helpers ----------

function h(tag, attributes = {}, ...children) {
  const element = document.createElement(tag);
  for (const [key, value] of Object.entries(attributes)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") element.className = value;
    else if (key.startsWith("on")) element.addEventListener(key.slice(2), value);
    else if (value === true) element.setAttribute(key, "");
    else element.setAttribute(key, String(value));
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    element.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return element;
}

function toast(message) {
  const element = document.getElementById("toast");
  element.textContent = message;
  element.classList.add("visible");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => element.classList.remove("visible"), 2600);
}

function errorText(error) {
  const code = String(error);
  if (code.startsWith("invalid:")) return code.slice("invalid:".length);
  const key = `error.${code}`;
  return strings[key] ? t(key) : t("error.generic");
}

async function call(command, args = {}) {
  try {
    return await invoke(command, args);
  } catch (error) {
    if (String(error) === "locked") {
      state.unlocked = false;
      render();
    }
    toast(errorText(error));
    throw error;
  }
}

// ---------- Data ----------

async function refreshInfo() {
  state.info = await invoke("app_info");
  renderTopbar();
}

async function refreshPending() {
  state.pending = await invoke("pending_approvals");
  renderNav();
  if (state.tab === "requests") render();
}

async function refreshManaged() {
  state.unlocked = await invoke("session_status");
  if (!state.unlocked) return;
  if (state.tab === "credentials") state.credentials = await invoke("list_credentials");
  if (state.tab === "activity") state.records = await invoke("access_records");
}

async function refreshClients() {
  state.clients = await invoke("client_status");
}

// ---------- Top bar and navigation ----------

function renderTopbar() {
  const info = state.info;
  if (!info) return;
  const dev = document.getElementById("dev-badge");
  dev.hidden = !info.development;
  dev.textContent = t("dev");
  const toggle = document.getElementById("access-toggle");
  toggle.textContent = info.agentAccessPaused ? t("access.paused") : t("access.active");
  toggle.title = info.agentAccessPaused ? t("access.resumeHint") : t("access.pauseHint");
  toggle.classList.toggle("pill-paused", info.agentAccessPaused);
  const lock = document.getElementById("lock-button");
  lock.hidden = !state.unlocked;
  lock.textContent = t("lock");
}

const TABS = ["requests", "credentials", "activity", "agents", "settings"];

function renderNav() {
  const nav = document.getElementById("nav");
  nav.replaceChildren(
    ...TABS.map((tab) =>
      h(
        "button",
        {
          type: "button",
          class: `nav-item${state.tab === tab ? " active" : ""}`,
          "aria-current": state.tab === tab ? "page" : null,
          onclick: () => switchTab(tab),
        },
        h("span", {}, t(`nav.${tab}`)),
        tab === "requests" && state.pending.length > 0 ? h("span", { class: "count" }, state.pending.length) : null,
      ),
    ),
  );
}

async function switchTab(tab) {
  state.tab = tab;
  renderNav();
  try {
    await refreshManaged();
    if (tab === "agents") await refreshClients();
  } catch (_) {
    // Shown by the view.
  }
  render();
}

// ---------- Views ----------

function lockedView() {
  return h(
    "section",
    { class: "empty" },
    h("div", { class: "empty-mark" }, "🔒"),
    h("h2", {}, t("locked.title")),
    h("p", { class: "muted" }, t("locked.body")),
    h(
      "button",
      {
        type: "button",
        class: "primary",
        onclick: async () => {
          await call("unlock");
          state.unlocked = true;
          await refreshManaged();
          render();
        },
      },
      t("locked.unlock"),
    ),
  );
}

function remaining(expiresAt) {
  const seconds = Math.max(0, expiresAt - Math.floor(Date.now() / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

// The caller name and purpose are typed by the agent; mark them wherever
// they appear.
function unverified() {
  return h("span", { class: "badge badge-warn", title: t("requests.unverified") }, t("requests.unverifiedBadge"));
}

function requestCard(request) {
  const timer = h("span", { class: "timer", "data-expires": request.expiresAt }, remaining(request.expiresAt));
  return h(
    "article",
    { class: "card request" },
    h("header", { class: "request-head" },
      request.callerName
        ? [h("span", { class: "caller" }, request.callerName), unverified()]
        : h("span", { class: "caller" }, t("requests.agent")),
      h("span", { class: "muted" }, t("requests.wants")),
      h("span", { class: "spacer" }),
      h("span", { class: "muted small" }, t("requests.expires"), " ", timer),
    ),
    // The program that will start, as the Broker resolved it; the command
    // line below is what the agent typed.
    h("p", { class: "program" },
      h("span", { class: "chip-label" }, t("requests.program")),
      h("span", { class: "mono" }, request.executable),
    ),
    h("pre", { class: "command" }, request.commandLine),
    h("p", { class: "muted small mono" }, t("requests.in"), " ", request.workingDirectory),
    request.callerPurpose
      ? h("p", { class: "purpose" },
          h("span", { class: "chip-label" }, t("requests.purpose")),
          h("span", {}, request.callerPurpose),
          unverified(),
        )
      : null,
    h("div", { class: "chips" },
      h("span", { class: "chip-label" }, t("requests.credentials")),
      request.credentialNames.map((name) => h("span", { class: "chip" }, name)),
    ),
    request.environmentVariables.length
      ? h("div", { class: "chips" },
          h("span", { class: "chip-label" }, t("requests.deliveredAs")),
          request.environmentVariables.map((name) => h("span", { class: "chip mono" }, name)),
        )
      : null,
    h("footer", { class: "actions" },
      h("button", { type: "button", class: "secondary", onclick: () => decide(request.requestID, false) }, t("requests.deny")),
      h("button", { type: "button", class: "primary", onclick: () => decide(request.requestID, true) }, t("requests.approve")),
    ),
  );
}

async function decide(requestId, approve) {
  try {
    await call("decide", { requestId, approve });
  } finally {
    await refreshPending();
  }
}

function requestsView() {
  if (state.pending.length === 0) {
    return h("section", { class: "empty" },
      h("div", { class: "empty-mark" }, "✓"),
      h("h2", {}, t("requests.empty")),
      h("p", { class: "muted" }, t("requests.emptyHint")),
    );
  }
  return h("section", { class: "stack" }, state.pending.map(requestCard));
}

function permissionSelect(credential) {
  const select = h(
    "select",
    {
      class: "permission",
      "aria-label": t("editor.permission"),
      onchange: async (event) => {
        const permission = event.target.value;
        try {
          await call("set_permission", { id: credential.id, permission });
          toast(t("toast.saved"));
        } catch (_) {
          event.target.value = credential.permission;
          return;
        }
        state.credentials = await invoke("list_credentials");
        render();
      },
    },
    ["ask", "allow", "hidden"].map((value) =>
      h("option", { value, selected: credential.permission === value, title: t(`permission.${value}Hint`) }, t(`permission.${value}`)),
    ),
  );
  return select;
}

function credentialRow(credential) {
  const now = Date.now() / 1000;
  const expired = credential.expiresAt && credential.expiresAt <= now;
  return h(
    "article",
    { class: "card credential" },
    h("div", { class: "credential-main" },
      h("div", { class: "credential-title" },
        h("span", { class: "credential-name" }, credential.name),
        expired ? h("span", { class: "badge badge-warn" }, t("credentials.expired")) : null,
      ),
      credential.usageInstructions ? h("p", { class: "muted small clamp" }, credential.usageInstructions) : null,
      h("div", { class: "chips" },
        credential.components.map((component) =>
          h("span", { class: "chip" }, component.name, component.environmentVariable ? h("span", { class: "mono muted" }, ` → ${component.environmentVariable}`) : null),
        ),
      ),
    ),
    h("div", { class: "credential-side" },
      permissionSelect(credential),
      h("div", { class: "row-actions" },
        h("button", { type: "button", class: "ghost", onclick: () => openEditor(credential) }, t("credentials.edit")),
        h("button", { type: "button", class: "ghost", onclick: () => reveal(credential) }, t("credentials.reveal")),
        h("button", { type: "button", class: "ghost danger", onclick: () => removeCredential(credential) }, t("credentials.delete")),
      ),
    ),
  );
}

function credentialsView() {
  if (!state.unlocked) return lockedView();
  const toolbar = h("div", { class: "toolbar" },
    h("button", { type: "button", class: "primary", onclick: () => openEditor(null) }, t("credentials.new")),
  );
  if (state.credentials.length === 0) {
    return h("section", {}, toolbar,
      h("div", { class: "empty" },
        h("h2", {}, t("credentials.empty")),
        h("p", { class: "muted" }, t("credentials.emptyHint")),
      ),
    );
  }
  const groups = new Map();
  for (const credential of state.credentials) {
    const group = credential.group || "";
    if (!groups.has(group)) groups.set(group, []);
    groups.get(group).push(credential);
  }
  const ordered = [...groups.keys()].sort((a, b) => (a === "" ? 1 : b === "" ? -1 : a.localeCompare(b)));
  return h("section", {}, toolbar,
    ordered.map((group) =>
      h("div", { class: "group" },
        h("h3", { class: "group-title" }, group || t("credentials.ungrouped")),
        h("div", { class: "stack" }, groups.get(group).map(credentialRow)),
      ),
    ),
  );
}

function activityView() {
  if (!state.unlocked) return lockedView();
  if (state.records.length === 0) {
    return h("section", { class: "empty" }, h("h2", {}, t("activity.empty")));
  }
  const format = new Intl.DateTimeFormat(language === "zh" ? "zh-CN" : undefined, { dateStyle: "medium", timeStyle: "short" });
  return h("section", {},
    h("table", { class: "table" },
      h("thead", {}, h("tr", {},
        ["when", "credentials", "program", "caller", "decision"].map((key) => h("th", {}, t(`activity.${key}`))),
      )),
      h("tbody", {}, state.records.map((record) =>
        h("tr", {},
          h("td", { class: "nowrap" }, format.format(new Date(record.at * 1000))),
          h("td", {}, record.credentialNames.join(", ")),
          h("td", { class: "mono" }, record.executable),
          h("td", {}, record.callerName || "—"),
          h("td", {}, h("span", { class: `decision decision-${record.decision}` }, t(`decision.${record.decision}`))),
        ),
      )),
    ),
  );
}

function copyButton(text) {
  return h("button", {
    type: "button",
    class: "ghost small",
    onclick: async () => {
      await navigator.clipboard.writeText(text);
      toast(t("toast.copied"));
    },
  }, t("reveal.copy"));
}

function agentsView() {
  const helper = state.info?.helperPath ?? "askkey.exe";
  const cli = `"${helper}" run --wait-for-approval --credential "<name>" --operation-id <id> --caller-name "<agent>" -- <program.exe> <args>`;
  return h("section", { class: "stack" },
    h("p", { class: "muted" }, t("agents.intro")),
    h("div", { class: "card" },
      h("div", { class: "label" }, t("agents.helper")),
      h("div", { class: "code-row" }, h("code", { class: "mono" }, helper), copyButton(helper)),
    ),
    state.clients.map((client) =>
      h("article", { class: "card client" },
        h("div", { class: "client-head" },
          h("span", { class: "client-name" }, t(`client.${client.client}`)),
          h("span", { class: `status ${client.configured ? "status-on" : "status-off"}` },
            client.configured ? t("agents.connected") : t("agents.notConnected")),
          h("span", { class: "spacer" }),
          h("button", {
            type: "button",
            class: client.configured ? "secondary" : "primary",
            onclick: async () => {
              if (!state.unlocked) {
                await call("unlock");
                state.unlocked = true;
                renderTopbar();
              }
              await call("connect_client", { client: client.client });
              toast(t("toast.connected"));
              await refreshClients();
              render();
            },
          }, client.configured ? t("agents.reconnect") : t("agents.connect")),
        ),
        h("p", { class: "muted small mono" }, client.configPath),
        h("details", { class: "details" },
          h("summary", {}, t("agents.manual")),
          h("div", { class: "code-row" }, h("pre", { class: "command" }, client.manual), copyButton(client.manual)),
        ),
      ),
    ),
    h("div", { class: "card" },
      h("div", { class: "label" }, t("agents.cli")),
      h("p", { class: "muted small" }, t("agents.cliHint")),
      h("div", { class: "code-row" }, h("pre", { class: "command" }, cli), copyButton(cli)),
    ),
  );
}

function settingsView() {
  const info = state.info;
  const toggle = h("input", {
    type: "checkbox",
    checked: info.approvalRequiresAuth,
    onchange: async (event) => {
      try {
        if (!state.unlocked) {
          await call("unlock");
          state.unlocked = true;
        }
        await call("set_approval_requires_auth", { required: event.target.checked });
        toast(t("toast.saved"));
      } catch (_) {
        event.target.checked = !event.target.checked;
      }
      await refreshInfo();
    },
  });
  return h("section", { class: "stack" },
    h("label", { class: "card setting" },
      toggle,
      h("span", {}, h("strong", {}, t("settings.approvalAuth")), h("br"), h("span", { class: "muted small" }, t("settings.approvalAuthHint"))),
    ),
    h("div", { class: "card" },
      h("dl", { class: "pairs" },
        h("dt", {}, t("settings.data")), h("dd", { class: "mono" }, info.dataDirectory),
        h("dt", {}, t("settings.version")), h("dd", {}, info.version),
      ),
    ),
    h("p", { class: "muted small" }, t("settings.security")),
  );
}

function render() {
  renderTopbar();
  const view = document.getElementById("view");
  const builders = {
    requests: requestsView,
    credentials: credentialsView,
    activity: activityView,
    agents: agentsView,
    settings: settingsView,
  };
  if (!state.info) return;
  view.replaceChildren(builders[state.tab]());
}

// ---------- Wiring ----------

document.getElementById("access-toggle").addEventListener("click", async () => {
  try {
    await call("set_agent_access_paused", { paused: !state.info.agentAccessPaused });
  } catch (_) {
    // Toast shown.
  }
  await refreshInfo();
  await refreshPending();
});

document.getElementById("lock-button").addEventListener("click", async () => {
  await invoke("lock");
  state.unlocked = false;
  state.credentials = [];
  state.records = [];
  render();
});

setInterval(() => {
  for (const timer of document.querySelectorAll(".timer")) {
    timer.textContent = remaining(Number(timer.dataset.expires));
  }
}, 1000);

// Expired requests drop out of the list.
setInterval(refreshPending, 5000);

listen("approvals-changed", async () => {
  await refreshPending();
  if (state.pending.length > 0 && state.tab !== "requests") switchTab("requests");
});
listen("settings-changed", refreshInfo);

(async () => {
  await refreshInfo();
  await refreshPending();
  render();
  renderNav();
})();
