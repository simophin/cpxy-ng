"use strict";

const PAGE_SIZE = 200;
const MAX_ROWS = 2000;
const POLL_MS = 1000;
const ui = Object.fromEntries(["status", "sent", "received", "count", "older", "follow", "latest", "message", "connections"].map(id => [id, document.getElementById(id)]));
let session = null;
let records = [];
let oldestSeq = 0;
let busy = false;
let timer;
let request;
let notice = "";

function bytes(value) {
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
  return `${value.toLocaleString(undefined, { maximumFractionDigits: unit ? 1 : 0 })} ${units[unit]}`;
}

async function get(path) {
  request = new AbortController();
  const timeout = setTimeout(() => request.abort(), 10000);
  try {
    const response = await fetch(path, { cache: "no-store", signal: request.signal });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return await response.json();
  } finally { clearTimeout(timeout); request = null; }
}

function adoptSession(value) {
  if (session === value) return false;
  if (session !== null) notice = "Router worker restarted. History and totals have reset.";
  session = value;
  records = [];
  oldestSeq = 0;
  return true;
}

function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}

function render() {
  const fragment = document.createDocumentFragment();
  for (const { seq, event } of records) {
    const row = element("li", undefined, event.error === null ? "" : "failed");
    row.dataset.seq = seq;
    const date = new Date(event.time_millis);
    const time = element("time", date.toLocaleTimeString(undefined, { hour12: false }));
    time.dateTime = date.toISOString();
    time.title = date.toLocaleString();
    const destination = element("div", undefined, "destination");
    const host = event.host.includes(":") ? `[${event.host}]` : event.host;
    destination.append(element("div", `${host}:${event.port}`, "host"));
    const details = [];
    if (event.ip && event.ip !== event.host) details.push(event.ip);
    details.push(event.error === null ? `${event.delay_millis} ms` : `Failed: ${event.error}`);
    destination.append(element("div", details.join(" · "), "detail"));
    row.append(time, destination, element("span", event.outbound, "route"));
    fragment.append(row);
  }
  ui.connections.replaceChildren(fragment);
  ui.count.textContent = `${records.length.toLocaleString()} connections`;
  ui.older.disabled = busy || !records.length || records[0].seq <= oldestSeq;
  ui.message.textContent = notice || (records.length ? "" : "Waiting for connections…");
}

async function latest() {
  const page = await get(`/api/connections?limit=${PAGE_SIZE}`);
  adoptSession(page.session);
  records = page.records;
  oldestSeq = page.oldest_seq;
  return true;
}

async function newer() {
  if (!records.length) return latest();
  let changed = false;
  // Limit each poll's work so a busy router cannot monopolize the browser.
  for (let pageNumber = 0; pageNumber < 5; pageNumber++) {
    const since = records[records.length - 1].seq + 1;
    const page = await get(`/api/connections?since=${since}&limit=${PAGE_SIZE}`);
    if (adoptSession(page.session) || page.oldest_seq > since) {
      notice = "History changed while away. Showing the latest connections.";
      return latest();
    }
    oldestSeq = page.oldest_seq;
    records.push(...page.records);
    changed ||= page.records.length > 0;
    if (records.length > MAX_ROWS) records.splice(0, records.length - MAX_ROWS);
    if (page.records.length < PAGE_SIZE) break;
  }
  return changed;
}

async function update(mode = "poll") {
  if (busy || document.hidden) return;
  clearTimeout(timer);
  busy = true;
  ui.older.disabled = true;
  ui.latest.disabled = true;
  try {
    const stats = await get("/api/traffic");
    const restarted = adoptSession(stats.session);
    ui.sent.textContent = bytes(stats.sent);
    ui.received.textContent = bytes(stats.received);
    ui.status.textContent = stats.running ? "Connected to router" : "Packet engine stopped";
    ui.status.className = stats.running ? "" : "error";
    let changed = false;
    if (mode === "older" && !restarted && records.length) {
      const first = records[0].seq;
      const page = await get(`/api/connections?before=${first}&limit=${PAGE_SIZE}`);
      if (adoptSession(page.session)) await latest();
      else {
        oldestSeq = page.oldest_seq;
        records.unshift(...page.records);
        records.splice(MAX_ROWS);
        if (!page.records.length) notice = "Older connections have expired from the router’s history.";
      }
      changed = true;
    } else if (mode === "latest" || restarted) {
      await latest();
      changed = true;
    } else if (ui.follow.checked) {
      changed = await newer();
    }
    // Preserve the viewed connection when prepending or refreshing while browsing history.
    const anchorIndex = mode === "older" ? Array.from(ui.connections.children).findIndex(row => row.getBoundingClientRect().bottom > ui.connections.getBoundingClientRect().top) : -1;
    const anchor = anchorIndex >= 0 ? ui.connections.children[anchorIndex] : null;
    const top = anchor?.getBoundingClientRect().top;
    const anchorSeq = anchor?.dataset.seq;
    if (changed) {
      render();
      if (anchorSeq) {
        const index = records.findIndex(record => String(record.seq) === anchorSeq);
        if (index >= 0) ui.connections.scrollTop += ui.connections.children[index].getBoundingClientRect().top - top;
        else ui.connections.scrollTop = 0;
      } else if (ui.follow.checked) ui.connections.scrollTop = ui.connections.scrollHeight;
    }
  } catch (error) {
    if (!document.hidden) {
      ui.status.textContent = `Router unavailable (${error.message}). Retrying…`;
      ui.status.className = "error";
    }
  } finally {
    busy = false;
    ui.latest.disabled = false;
    ui.older.disabled = !records.length || records[0].seq <= oldestSeq;
    if (!document.hidden) timer = setTimeout(() => update(), POLL_MS);
  }
}

ui.older.addEventListener("click", () => {
  if (busy) return;
  ui.follow.checked = false;
  notice = "";
  update("older");
});
ui.latest.addEventListener("click", () => {
  if (busy) return;
  ui.follow.checked = true;
  notice = "";
  update("latest");
});
ui.follow.addEventListener("change", () => {
  notice = "";
  if (ui.follow.checked) update("latest");
});
ui.connections.addEventListener("scroll", () => {
  if (ui.connections.scrollHeight - ui.connections.clientHeight - ui.connections.scrollTop > 100) ui.follow.checked = false;
}, { passive: true });
document.addEventListener("visibilitychange", () => {
  clearTimeout(timer);
  if (document.hidden) request?.abort();
  else update();
});
update();
