import purify from "/vendor/purify.mjs";
import { adaptCssColor } from "/ui/theme-color.js";
import { renderCodeView } from "/ui/code.js";

const $ = id => document.getElementById(id);
const reader = $("reader");
const scroll = $("scroll");
const key = location.hash.slice(1);
const headers = { "x-hayaku-key": key };
const colors = ["yellow", "green", "blue", "pink"];
let book = null;
let pdf = null;
let contents = [];
let currentTab = "contents";
let openBusy = false;
let sideRevision = 0;
let searchRevision = 0;
let searchIndex = -1;
let lastSearchQuery = "";
let statusTimer;
let saveTimer;
let saveQueue = Promise.resolve();
let picked = null;
let editing = null;
let restoring = false;
let progressFrame;
let results = [];
const documentTabs = new Map();
const isDesktop = new URLSearchParams(location.search).has("desktop");
const english = !navigator.language.toLowerCase().startsWith("zh");
const translate = (zh, en) => english ? en : zh;
let installation;
let updateInProgress = false;
let workspaceQueue = Promise.resolve();
let started = false;
let installDismissed = false;
let drainingDrops = false;
let rawBookRules = [];
let lastSmartInvert = false;
const textEncoder = new TextEncoder();

function isSmartInvertActive() {
  return $("theme").value === "dark" && $("smart-invert").checked;
}

async function request(url, options = {}) {
  const response = await fetch(url, { ...options, headers: { ...headers, ...options.headers } });
  if (!response.ok) throw new Error(await response.text());
  return response;
}

function post(url, payload, keepalive = false) {
  const body = JSON.stringify(payload);
  return request(url, {
    method: "POST",
    keepalive: keepalive && textEncoder.encode(body).length < 60000,
    headers: { "Content-Type": "application/json" },
    body
  });
}

function message(text = "", duration = 3200) {
  clearTimeout(statusTimer);
  $("status").textContent = text;
  if (text && duration > 0) {
    statusTimer = setTimeout(() => { $("status").textContent = ""; }, duration);
  }
}

function report(error) {
  console.error(error);
  message(error.name === "PasswordException" ? "此 PDF 受密码保护。" : error.message || String(error), 0);
}
$("status").addEventListener("click", () => message(""));

function sessionUrl(action, target = book) {
  return "/api/" + action + "/" + target.session;
}

function getState() {
  return book?.saved;
}

function save(keepalive = false) {
  clearTimeout(saveTimer);
  if (!book) return Promise.resolve();
  if (!restoring && !reader.hidden && reader.childElementCount) {
    getState().position = activePosition(true);
    getState().digest = book.digest;
  }
  const url = sessionUrl("save");
  const value = structuredClone(getState());
  saveQueue = saveQueue.catch(() => {}).then(() => post(url, value, keepalive));
  saveQueue.catch(report);
  return saveQueue;
}

function scheduleSave() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(save, 250);
}

function unit(index) {
  return reader.querySelector('[data-unit="' + index + '"]');
}

function units() {
  return Array.from(reader.querySelectorAll("#reader > [data-unit]"));
}

function contentRoute(index, encoding = "", raw = false) {
  const query = new URLSearchParams({ chapter: index });
  if (encoding) query.set("encoding", encoding);
  if (raw) query.set("raw", "true");
  return sessionUrl("content") + "?" + query;
}

function stripRemote(reference, base) {
  if (!reference || /^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(reference) || reference.includes("\\")) return null;
  try {
    const origin = "https://hayaku.invalid/";
    const resolved = new URL(reference, new URL(base.split("/").map(encodeURIComponent).join("/"), origin));
    if (resolved.origin !== new URL(origin).origin) return null;
    return { path: decodeURIComponent(resolved.pathname.slice(1)), fragment: decodeURIComponent(resolved.hash.slice(1)) };
  } catch { return null; }
}

const safeStyle = new Set([
  "font-style", "font-weight", "text-decoration", "text-align", "vertical-align",
  "font-size", "line-height", "text-indent", "color", "background-color",
  "margin-top", "margin-bottom", "margin-left", "margin-right", "padding-left", "padding-right"
]);

function resourceUrl(path) {
  return "/resource/" + key + "/" + book.session + "?" + new URLSearchParams({ path });
}

function safePairs(style) {
  const pairs = [];
  for (const property of safeStyle) {
    const value = style.getPropertyValue(property);
    if (value && /^[a-z0-9 #.,()%+-]+$/i.test(value) && !/(?:url|var|attr)\s*\(/i.test(value)) {
      pairs.push([property, value]);
    }
  }
  return pairs;
}

async function readStyles(markup, base, index) {
  const template = document.createElement("template");
  template.innerHTML = markup;
  const rules = [];
  const scope = '#reader > [data-unit="' + index + '"]';
  for (const element of template.content.querySelectorAll('style,link[rel~="stylesheet"][href]')) {
    let css = element.textContent;
    if (element.tagName === "LINK") {
      const resolved = stripRemote(element.getAttribute("href"), base);
      if (!resolved?.path || !/\.css$/i.test(resolved.path)) continue;
      try { css = await (await request(resourceUrl(resolved.path))).text(); }
      catch { continue; }
    }
    const parsed = new CSSStyleSheet();
    try {
      parsed.replaceSync(css.replace(/@import[^;]*;/gi, ""));
    } catch { continue; }
    for (const rule of parsed.cssRules) {
      if (!(rule instanceof CSSStyleRule)) continue;
      if (/[{}@;]/.test(rule.selectorText)) continue;
      const pairs = safePairs(rule.style);
      if (!pairs.length) continue;
      const selector = rule.selectorText.replace(/#([^\s.#,:[\]>+~()]+)/g, "#user-content-$1");
      const scoped = /^(?:body|html|:root)$/i.test(selector) ? scope : scope + " :is(" + selector + ")";
      rules.push({ selector: scoped, pairs });
    }
  }
  return rules;
}

const bookStyles = new CSSStyleSheet();
document.adoptedStyleSheets = [...document.adoptedStyleSheets, bookStyles];

function applyFlowColors() {
  const invert = isSmartInvertActive();
  bookStyles.replaceSync("");
  for (const rule of rawBookRules) {
    const declarations = rule.pairs.map(([property, value]) => {
      if (!invert) return property + ":" + value;
      if (property === "color") return property + ":" + adaptCssColor(value, false);
      if (property === "background-color") return property + ":" + adaptCssColor(value, true);
      return property + ":" + value;
    });
    try {
      bookStyles.insertRule(rule.selector + "{" + declarations.join(";") + "}", bookStyles.cssRules.length);
    } catch { /* Skip malformed or unsupported rules. */ }
  }
  for (const element of reader.querySelectorAll("[data-orig-color]")) {
    const original = element.dataset.origColor;
    element.style.color = invert ? adaptCssColor(original, false) : original;
  }
  for (const element of reader.querySelectorAll("[data-orig-bg]")) {
    const original = element.dataset.origBg;
    element.style.backgroundColor = invert ? adaptCssColor(original, true) : original;
  }
}

function addMarkup(target, markup, base, kind, index) {
  const parsed = purify.sanitize(markup, {
    RETURN_DOM_FRAGMENT: true, USE_PROFILES: { html: true },
    FORBID_TAGS: ["style", "link", "meta", "input", "button", "audio", "video", "source", "picture", "form", "iframe", "object", "embed"],
    FORBID_ATTR: ["srcset", "ping", "target", "autofocus", "contenteditable", "tabindex", "data-unit"],
    ALLOW_DATA_ATTR: false, SANITIZE_NAMED_PROPS: true
  });
  for (const element of parsed.querySelectorAll("[style]")) {
    const pairs = safePairs(element.style);
    element.removeAttribute("style");
    if (pairs.length) {
      element.setAttribute("style", pairs.map(([p, v]) => p + ":" + v).join(";"));
      for (const [property, value] of pairs) {
        if (property === "color") element.dataset.origColor = value;
        if (property === "background-color") element.dataset.origBg = value;
      }
    }
  }
  for (const image of parsed.querySelectorAll("img")) {
    const source = image.getAttribute("src");
    const resolved = stripRemote(source, base);
    if (resolved?.path) {
      image.src = resourceUrl(resolved.path);
      image.loading = "lazy";
    } else if (!/^data:image\/(?:png|jpeg|gif|webp);base64,/i.test(source || "")) {
      image.removeAttribute("src");
    }
  }
  for (const link of parsed.querySelectorAll("a[href]")) {
    const original = link.getAttribute("href");
    const resolved = stripRemote(original, base);
    link.removeAttribute("href");
    if (!resolved) continue;
    const isEbook = kind === "epub" || kind === "kindle";
    const chapter = isEbook ? book.chapters.findIndex(item => item.path === resolved.path) : index;
    if (chapter === -1 || (!isEbook && !original.startsWith("#"))) continue;
    link.href = "#";
    link.addEventListener("click", event => {
      event.preventDefault();
      jump(chapter, null, resolved.fragment);
    });
  }
  target.append(parsed);
}

function chapterHeading(title, index, fragment) {
  return { title, index, fragment };
}

function addHeadings(target, index) {
  const used = new Set();
  for (const heading of target.querySelectorAll("h1,h2,h3,h4,h5,h6")) {
    const title = heading.textContent.trim();
    if (!title) continue;
    const slug = title.toLowerCase().replace(/[^\p{L}\p{N}\s_-]/gu, "").trim().replace(/\s+/g, "-") || "heading";
    let id = heading.id || "user-content-" + slug;
    for (let count = 1; used.has(id); count++) id = "user-content-" + slug + "-" + count;
    used.add(id);
    heading.id = id;
    contents.push(chapterHeading("　".repeat(Number(heading.tagName[1]) - 1) + title, index, id));
  }
}

function updateViewModeButton() {
  const btn = $("view-mode");
  const canToggle = Boolean(book && (book.kind === "markdown" || book.kind === "html"));
  btn.hidden = !canToggle;
  if (!canToggle) return;
  const code = getState().viewMode === "code";
  btn.textContent = code ? translate("渲染模式", "Rendered") : translate("代码模式", "Code");
  btn.setAttribute("aria-pressed", String(code));
}

async function loadFlow() {
  rawBookRules = [];
  if (book.kind === "text") {
    reader.className = "flow";
    const pre = document.createElement("pre");
    pre.className = "plain-text";
    pre.dataset.unit = 0;
    pre.textContent = await (await request(contentRoute(0, getState().encoding))).text();
    reader.replaceChildren(pre);
    bookStyles.replaceSync("");
    return;
  }
  if ((book.kind === "markdown" || book.kind === "html") && getState().viewMode === "code") {
    reader.className = "flow code-mode";
    const source = await (await request(contentRoute(0, "", book.kind === "markdown"))).text();
    reader.replaceChildren(renderCodeView(source, book.kind));
    bookStyles.replaceSync("");
    return;
  }
  reader.className = "flow";
  const chapters = book.chapters.length ? book.chapters : [{ path: "", title: book.title }];
  const markups = new Array(chapters.length);
  let nextChapter = 0;
  await Promise.all(Array.from({ length: Math.min(8, chapters.length) }, async () => {
    while (nextChapter < chapters.length) {
      const index = nextChapter++;
      markups[index] = await (await request(contentRoute(index))).text();
    }
  }));
  const fragment = document.createDocumentFragment();
  const rules = [];
  if (book.outline?.length) contents.push(...book.outline);
  for (const [index, chapter] of chapters.entries()) {
    const part = document.createElement("section");
    part.dataset.unit = index;
    const markup = markups[index];
    rules.push(...await readStyles(markup, chapter.path, index));
    addMarkup(part, markup, chapter.path, book.kind, index);
    if (!book.outline?.length && book.chapters.length) contents.push(chapterHeading(chapter.title, index, ""));
    if (book.kind !== "epub" && !book.outline?.length) addHeadings(part, index);
    fragment.append(part);
  }
  rawBookRules = rules;
  reader.replaceChildren(fragment);
  applyFlowColors();
}

async function present(next) {
  const previous = book;
  if (previous) await save().catch(() => {});
  if (previous?.id === next.id && previous.session !== next.session) {
    post(sessionUrl("close", next), {}).catch(() => {});
    return;
  }
  if (previous && documentTabs.has(previous.id)) documentTabs.get(previous.id).lastUsed = Date.now();
  if (previous?.id === next.id) next.saved = previous.saved;
  pdf?.close();
  pdf = null;
  book = next;
  book.saved = { annotations: [], ...book.saved };
  contents = [];
  results = [];
  CSS.highlights?.delete("hayaku-search");
  searchRevision++;
  picked = null;
  $("selection-tools").hidden = true;
  $("reader").replaceChildren();
  rawBookRules = [];
  bookStyles.replaceSync("");
  $("reader").hidden = false;
  $("welcome").hidden = true;
  $("file-name").textContent = book.title;
  document.title = book.title + " · HayakuReader";
  $("progress").hidden = false;
  $("pdf-tools").hidden = !["pdf", "word"].includes(book.kind);
  $("encoding-label").hidden = book.kind !== "text";
  $("encoding").value = getState().encoding || "";
  updateViewModeButton();
  for (const id of ["search-toggle", "bookmark", "page-note"]) $(id).disabled = true;
  restoring = true;
  try {
    if (book.kind === "pdf") {
      reader.className = "fixed";
      const { openPdf } = await import("/ui/pdf.js");
      pdf = await openPdf(contentRoute(0), headers, reader, scroll, scheduleRefreshHighlights, isSmartInvertActive);
      contents = await pdf.outline().catch(() => []);
      $("page-number").max = pdf.count;
    } else if (book.kind === "word") {
      reader.className = "fixed";
      const { openWord } = await import("/ui/word.js");
      pdf = openWord(book.word, contentRoute, request, reader, scroll, scheduleRefreshHighlights, isSmartInvertActive);
      contents = book.word.outline;
      $("page-number").max = pdf.count;
    } else {
      await loadFlow();
    }
    const location = getState().position;
    if (location) await restorePosition(location);
    else scroll.scrollTop = 0;
    for (const id of ["search-toggle", "bookmark", "page-note"]) $(id).disabled = false;
    refreshHighlights();
    updateProgress();
    const side = contents.length ? "contents" : getState().annotations.length ? "notes" : null;
    setSidebar(Boolean(side));
    currentTab = side || "contents";
    const existingTab = documentTabs.get(book.id);
    if (existingTab?.document && existingTab.document.session !== book.session) {
      post(sessionUrl("close", existingTab.document), {}).catch(() => {});
    }
    documentTabs.set(book.id, { id: book.id, title: book.title, document: book, lastUsed: Date.now() });
    sleepTabs();
    renderDocumentTabs();
    persistWorkspace();
    await renderSidebar();
  } catch (error) {
    reader.hidden = true;
    $("progress").hidden = true;
    $("welcome").hidden = false;
    reader.replaceChildren();
    pdf?.close();
    pdf = null;
    post(sessionUrl("close"), {}).catch(() => {});
    if (documentTabs.has(book.id)) documentTabs.get(book.id).document = null;
    book = null;
    $("file-name").textContent = "";
    $("view-mode").hidden = true;
    document.title = "HayakuReader";
    renderDocumentTabs();
    throw error;
  } finally {
    restoring = false;
  }
}

async function openDocument(payload = {}, file = null) {
  if (openBusy) return false;
  openBusy = true;
  for (const id of ["open-file", "welcome-open"]) $(id).disabled = true;
  message(translate("正在打开文件…", "Opening file…"), 0);
  try {
    const cached = payload.id && documentTabs.get(payload.id)?.document;
    if (cached && cached.id === book?.id) return true;
    const response = file
      ? await request("/api/import?" + new URLSearchParams({ name: file.name }), { method: "POST", body: file })
      : cached ? null : await post("/api/open", payload);
    const next = cached ?? await response.json();
    if (next) await present(next);
    message();
    return Boolean(next);
  } catch (error) {
    report(error);
  } finally {
    for (const id of ["open-file", "welcome-open"]) $(id).disabled = false;
    openBusy = false;
  }
}

function setSidebar(visible) {
  $("sidebar").hidden = !visible;
  $("toggle-sidebar").setAttribute("aria-expanded", String(visible));
}

function sleepTabs() {
  const ordered = [...documentTabs.values()].sort((left, right) => right.lastUsed - left.lastUsed);
  const expire = Date.now() - 5 * 60 * 1000;
  let changed = false;
  for (const [index, entry] of ordered.entries()) {
    if (entry.id === book?.id || !entry.document || (index < 5 && entry.lastUsed > expire)) continue;
    post(sessionUrl("close", entry.document), {}).catch(() => {});
    entry.document = null;
    changed = true;
  }
  return changed;
}

function renderDocumentTabs() {
  const host = $("document-tabs");
  host.replaceChildren();
  host.hidden = !documentTabs.size;
  for (const entry of documentTabs.values()) {
    const item = document.createElement("div");
    item.className = "document-tab";
    const active = entry.id === book?.id;
    item.dataset.sleeping = String(!entry.document);
    item.dataset.active = String(active);
    const select = document.createElement("button");
    select.setAttribute("role", "tab");
    select.setAttribute("aria-selected", String(active));
    select.textContent = entry.title;
    select.title = entry.title + (!entry.document ? translate("（已休眠，点击恢复）", " (Sleeping, click to resume)") : "");
    select.onclick = () => openDocument({ id: entry.id });
    const close = document.createElement("button");
    close.textContent = "×";
    close.setAttribute("aria-label", translate("关闭 ", "Close ") + entry.title);
    close.onclick = () => closeDocumentTab(entry).catch(report);
    item.addEventListener("mousedown", event => { if (event.button === 1) event.preventDefault(); });
    item.addEventListener("auxclick", event => {
      if (event.button === 1) {
        event.preventDefault();
        closeDocumentTab(entry).catch(report);
      }
    });
    item.append(select, close);
    host.append(item);
  }
}

async function closeDocumentTab(entry) {
  if (openBusy) return;
  if (entry.id === book?.id) {
    await save().catch(() => {});
    pdf?.close();
    pdf = null;
    book = null;
    contents = [];
    reader.replaceChildren();
    reader.hidden = true;
    $("welcome").hidden = false;
    $("progress").hidden = true;
    $("file-name").textContent = "";
    $("selection-tools").hidden = true;
    $("search-form").hidden = true;
    $("view-mode").hidden = true;
    rawBookRules = [];
    document.title = "HayakuReader";
    setSidebar(false);
    for (const id of ["search-toggle", "bookmark", "page-note"]) $(id).disabled = true;
  }
  documentTabs.delete(entry.id);
  if (entry.document) await post(sessionUrl("close", entry.document), {}).catch(() => {});
  if (!book && documentTabs.size) {
    const next = [...documentTabs.values()].sort((left, right) => right.lastUsed - left.lastUsed)[0];
    await openDocument({ id: next.id });
  }
  renderDocumentTabs();
  if (!book) renderWelcomeRecent().catch(() => {});
  await persistWorkspace();
}

function persistWorkspace(keepalive = false) {
  const tabs = [...documentTabs.values()].map(({ id, title, lastUsed }) => ({ id, title, lastUsed }));
  const value = { tabs, active: book?.id ?? null };
  workspaceQueue = workspaceQueue.catch(() => {}).then(() => post("/api/workspace", value, keepalive));
  return workspaceQueue.catch(report);
}

setInterval(() => { if (sleepTabs()) renderDocumentTabs(); }, 15000);

function activePosition(includeAnchor = false) {
  const top = scroll.getBoundingClientRect().top + 12;
  const candidates = units();
  let low = 0;
  let high = candidates.length - 1;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    if (candidates[middle].getBoundingClientRect().bottom < top) low = middle + 1;
    else high = middle;
  }
  const element = candidates[low];
  if (!element) return { index: 0, fraction: 0 };
  const box = element.getBoundingClientRect();
  const position = {
    index: Number(element.dataset.unit),
    fraction: Math.max(0, Math.min(1, (top - box.top) / Math.max(box.height, 1)))
  };
  if (includeAnchor && !pdf && position.fraction > 0) {
    const y = Math.min(top, box.bottom - 1);
    const x = Math.max(box.left + 28, 1);
    let caretContainer = null;
    let caretOffset = 0;
    if (document.caretRangeFromPoint) {
      const caret = document.caretRangeFromPoint(x, y);
      if (caret) {
        caretContainer = caret.startContainer;
        caretOffset = caret.startOffset;
      }
    } else if (document.caretPositionFromPoint) {
      const pos = document.caretPositionFromPoint(x, y);
      if (pos) {
        caretContainer = pos.offsetNode;
        caretOffset = pos.offset;
      }
    }
    if (caretContainer && element.contains(caretContainer)) {
      const prefix = document.createRange();
      prefix.selectNodeContents(element);
      prefix.setEnd(caretContainer, caretOffset);
      const start = prefix.toString().length;
      const text = element.textContent;
      const quote = text.slice(start, start + 32);
      if (quote.trim()) position.anchor = {
        index: position.index, start, quote, digest: book.digest,
        before: text.slice(Math.max(0, start - 24), start),
        after: text.slice(start + quote.length, start + quote.length + 24)
      };
    }
  }
  return position;
}

function updateProgress() {
  if (!book) return;
  const position = activePosition();
  const available = scroll.scrollHeight - scroll.clientHeight;
  const percent = available > 0 ? Math.round(100 * scroll.scrollTop / available) : 100;
  if (pdf) {
    $("location").textContent = "第 " + (position.index + 1) + " 页 / 共 " + pdf.count + " 页";
    if (document.activeElement !== $("page-number")) $("page-number").value = position.index + 1;
  } else {
    const chapter = book.chapters[position.index]?.title;
    $("location").textContent = [chapter, percent + "%"].filter(Boolean).join(" · ");
  }
  if (!restoring && reader.childElementCount) {
    scheduleSave();
  }
  syncActiveToc(position.index);
}

async function jump(index, part = null, fragment = "", fraction = 0) {
  const element = unit(index);
  if (!element) return false;
  if (pdf) await pdf.render(index);
  const range = part ? locate(part) : null;
  if (part && !range) return false;
  const anchor = fragment
    ? Array.from(element.querySelectorAll("[id]")).find(node => node.id === fragment || node.id === "user-content-" + fragment)
    : null;
  const box = element.getBoundingClientRect();
  const top = range?.getBoundingClientRect().top ?? anchor?.getBoundingClientRect().top ?? box.top + fraction * box.height;
  scroll.scrollTop += top - scroll.getBoundingClientRect().top - 12;
  return true;
}

async function restorePosition(position, allowFraction = true) {
  if (position.anchor && await jump(position.index, position.anchor)) return true;
  return allowFraction && await jump(position.index, null, "", position.fraction);
}

function changeZoom(update) {
  if (!pdf) return;
  const { index, fraction } = activePosition();
  update();
  jump(index, null, "", fraction).catch(report);
}

function textRoot(element) {
  return pdf ? element.querySelector(".textLayer, .word-text") : element;
}

function rangeAt(root, start, end) {
  if (start < 0 || end <= start) return null;
  const walk = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let from = null;
  let offset = 0;
  while (walk.nextNode()) {
    const node = walk.currentNode;
    const after = offset + node.length;
    if (!from && start < after) from = [node, start - offset];
    if (from && end <= after) {
      const range = document.createRange();
      range.setStart(...from);
      range.setEnd(node, end - offset);
      return range;
    }
    offset = after;
  }
  return null;
}

function locate(part) {
  const element = unit(part.index);
  const root = element && textRoot(element);
  if (!root) return null;
  const text = root.textContent;
  let start = part.start;
  if (text.slice(start, start + part.quote.length) !== part.quote || (part.digest && part.digest !== book.digest)) {
    const candidates = [];
    for (let index = text.indexOf(part.quote); index >= 0; index = text.indexOf(part.quote, index + part.quote.length)) {
      const before = text.slice(Math.max(0, index - part.before.length), index);
      const after = text.slice(index + part.quote.length, index + part.quote.length + part.after.length);
      const context = Number(Boolean(part.before) && before.endsWith(part.before)) + Number(Boolean(part.after) && after.startsWith(part.after));
      candidates.push({ index, context });
      if (candidates.length > 1000) return null;
    }
    if (!candidates.length) return null;
    candidates.sort((a, b) => b.context - a.context);
    if (candidates.length > 1 && candidates[0].context === candidates[1].context) return null;
    start = candidates[0].index;
  }
  return rangeAt(root, start, start + part.quote.length);
}

function selectionParts(selection) {
  if (!book || !selection.rangeCount || selection.isCollapsed || !reader.contains(selection.anchorNode) || !reader.contains(selection.focusNode)) return [];
  const range = selection.getRangeAt(0);
  const parts = [];
  for (const element of units()) {
    const root = textRoot(element);
    if (!root || !range.intersectsNode(root)) continue;
    const prefix = document.createRange();
    prefix.selectNodeContents(root);
    const selected = prefix.cloneRange();
    if (root.contains(range.startContainer)) {
      prefix.setEnd(range.startContainer, range.startOffset);
      selected.setStart(range.startContainer, range.startOffset);
    } else {
      prefix.collapse(true);
    }
    if (root.contains(range.endContainer)) selected.setEnd(range.endContainer, range.endOffset);
    const quote = selected.toString();
    if (!quote.trim()) continue;
    const text = root.textContent;
    const start = prefix.toString().length;
    parts.push({
      index: Number(element.dataset.unit), start, quote, digest: book.digest,
      before: text.slice(Math.max(0, start - 24), start),
      after: text.slice(start + quote.length, start + quote.length + 24)
    });
  }
  return parts;
}

let highlightFrame;

function scheduleRefreshHighlights() {
  if (!book || !getState().annotations.length || highlightFrame) return;
  highlightFrame = requestAnimationFrame(() => {
    highlightFrame = null;
    refreshHighlights();
  });
}

function refreshHighlights() {
  if (!CSS.highlights || !book) return;
  if (!getState().annotations.length) {
    for (const color of colors) CSS.highlights.delete("hayaku-" + color);
    return;
  }
  const groups = new Map(colors.map(color => [color, new Highlight()]));
  for (const annotation of getState().annotations) {
    const group = groups.get(annotation.color || "yellow");
    if (!group) continue;
    for (const part of annotation.parts ?? []) {
      const range = locate(part);
      if (range) group.add(range);
    }
  }
  for (const [color, ranges] of groups) CSS.highlights.set("hayaku-" + color, ranges);
}

function selectionToolbar() {
  const selection = getSelection();
  const parts = selectionParts(selection);
  if (!parts.length) {
    $("selection-tools").hidden = true;
    return;
  }
  const bounds = selection.getRangeAt(0).getBoundingClientRect();
  picked = { parts, text: book.kind === "word" ? parts.map(part => part.quote).join("\n") : selection.toString() };
  const tools = $("selection-tools");
  tools.hidden = false;
  tools.style.left = Math.max(6, Math.min(bounds.left, innerWidth - tools.offsetWidth - 6)) + "px";
  tools.style.top = Math.max(6, bounds.top - tools.offsetHeight - 6) + "px";
}

function addAnnotation(annotation) {
  getState().annotations.push({ id: crypto.randomUUID(), ...annotation });
  document.getSelection()?.removeAllRanges();
  refreshHighlights();
  save();
  if (annotation.kind !== "highlight") {
    currentTab = "notes";
    setSidebar(true);
  }
  if (!$("sidebar").hidden && currentTab === "notes") {
    renderSidebar().catch(report);
  }
  message(translate("已保存批注。", "Annotation saved."));
}

function showEditor(annotation, existing = false) {
  editing = { annotation, existing };
  const dialog = $("editor");
  const kind = annotation.kind;
  $("editor-title").textContent = { highlight: "高亮颜色", note: "笔记", bookmark: "书签名称" }[kind];
  $("excerpt").textContent = annotation.parts?.map(part => part.quote).join("\n") || "";
  $("excerpt").hidden = !$("excerpt").textContent;
  $("note-field").hidden = kind === "highlight";
  $("color-field").hidden = kind === "bookmark";
  $("note-text").value = annotation.text || "";
  $("color").value = annotation.color || "yellow";
  $("delete-note").hidden = !existing;
  dialog.returnValue = "cancel";
  dialog.showModal();
}

function bookmarkTitle(position) {
  if (pdf) return "第 " + (position.index + 1) + " 页";
  const title = book.chapters[position.index]?.title;
  return title || "书签";
}

async function goAnnotation(annotation) {
  let located;
  if (annotation.parts?.length) {
    for (const part of annotation.parts) {
      if (await jump(part.index, part)) { located = true; break; }
    }
  } else {
    located = await restorePosition(annotation.position, !annotation.digest || annotation.digest === book.digest);
  }
  if (!located) {
    annotation.missing = true;
    save();
    await renderSidebar();
    message("原文位置已失效，摘录和笔记仍保留。");
  } else if (annotation.missing) {
    delete annotation.missing;
    save();
    await renderSidebar();
  }
}

function sideRow(title, action, extras = [], host = $("side-list")) {
  const row = document.createElement("div");
  row.className = "side-row";
  const button = document.createElement("button");
  button.textContent = title;
  button.title = title;
  button.addEventListener("click", () => Promise.resolve(action()).catch(report));
  row.append(button);
  for (const extra of extras) {
    const secondary = document.createElement("button");
    secondary.textContent = extra.label;
    if (extra.ariaLabel) secondary.setAttribute("aria-label", extra.ariaLabel);
    if (extra.title) secondary.title = extra.title;
    secondary.addEventListener("click", () => Promise.resolve(extra.action()).catch(report));
    row.append(secondary);
  }
  host.append(row);
  return button;
}

function sideMessage(text) {
  const item = document.createElement("p");
  item.className = "side-message";
  item.textContent = text;
  $("side-list").append(item);
}

function syncActiveToc(unitIndex) {
  if ($("sidebar").hidden || currentTab !== "contents") return;
  const buttons = Array.from($("side-list").querySelectorAll("button[data-unit]"));
  let active = null;
  for (const button of buttons) {
    button.removeAttribute("aria-current");
    if (Number(button.dataset.unit) <= unitIndex) active = button;
  }
  if (active) active.setAttribute("aria-current", "true");
}

async function deleteRecentItem(id) {
  await request("/api/recent/" + encodeURIComponent(id), { method: "DELETE" });
  await Promise.all([
    renderWelcomeRecent(),
    currentTab === "recent" ? renderSidebar() : Promise.resolve()
  ]);
}

async function clearRecentHistory() {
  await request("/api/recent", { method: "DELETE" });
  await Promise.all([
    renderWelcomeRecent(),
    currentTab === "recent" ? renderSidebar() : Promise.resolve()
  ]);
  message(translate("已清空阅读记录。", "Reading history cleared."));
}

async function renderWelcomeRecent() {
  const items = await (await request("/api/recent")).json();
  const section = $("welcome-recent");
  const list = $("welcome-recent-list");
  list.replaceChildren();
  section.hidden = !items.length;
  for (const item of items.slice(0, 5)) {
    const label = item.path.split(/[\\/]/).at(-1);
    sideRow(
      label,
      () => openDocument({ id: item.id }),
      [{
        label: "×",
        ariaLabel: translate("删除记录 ", "Remove ") + label,
        title: translate("删除记录", "Remove from history"),
        action: () => deleteRecentItem(item.id)
      }],
      list
    );
  }
}

function exportAnnotations() {
  if (!book) return;
  const lines = ["# " + book.title, ""];
  for (const item of getState().annotations) {
    const kind = { bookmark: translate("书签", "Bookmark"), note: translate("笔记", "Note"), highlight: translate("高亮", "Highlight") }[item.kind] || item.kind;
    lines.push("## " + kind + (item.color ? " (" + item.color + ")" : ""));
    const excerpt = item.parts?.map(part => part.quote).join("\n");
    if (excerpt) lines.push("> " + excerpt.replace(/\n/g, "\n> "));
    if (item.text) lines.push("", item.text);
    lines.push("");
  }
  const blob = new Blob([lines.join("\n")], { type: "text/markdown;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = book.title + ".annotations.md";
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  message(translate("已导出批注。", "Annotations exported."));
}

async function renderSidebar() {
  const revision = ++sideRevision;
  $("side-list").replaceChildren();
  for (const control of document.querySelectorAll("[data-tab]")) {
    control.setAttribute("aria-pressed", String(control.dataset.tab === currentTab));
  }
  if (currentTab === "recent") {
    const items = await (await request("/api/recent")).json();
    if (revision !== sideRevision) return;
    if (!items.length) sideMessage(translate("暂无阅读记录", "No recent files"));
    else {
      const actions = document.createElement("div");
      actions.className = "side-actions";
      const clear = document.createElement("button");
      clear.textContent = translate("清空记录", "Clear history");
      clear.addEventListener("click", () => clearRecentHistory().catch(report));
      actions.append(clear);
      $("side-list").append(actions);
    }
    for (const item of items) {
      const label = item.path.split(/[\\/]/).at(-1);
      sideRow(label, () => openDocument({ id: item.id }), [
        { label: translate("重新关联", "Relocate"), action: () => openDocument({ relocate: item.id }) },
        { label: "×", ariaLabel: translate("删除 ", "Remove ") + label, title: translate("删除记录", "Remove from history"), action: () => deleteRecentItem(item.id) }
      ]);
    }
  } else if (currentTab === "contents") {
    if (!contents.length) sideMessage(translate("暂无目录", "No table of contents"));
    for (const item of contents) {
      const button = sideRow(item.title, () => jump(item.index, null, item.fragment));
      button.dataset.unit = item.index;
    }
    if (book) syncActiveToc(activePosition().index);
  } else if (currentTab === "notes") {
    const annotations = book?.saved.annotations ?? [];
    if (!annotations.length) sideMessage(translate("暂无高亮、笔记或书签", "No highlights, notes, or bookmarks"));
    else {
      const actions = document.createElement("div");
      actions.className = "side-actions";
      const exportBtn = document.createElement("button");
      exportBtn.textContent = translate("导出批注 (.md)", "Export (.md)");
      exportBtn.addEventListener("click", exportAnnotations);
      actions.append(exportBtn);
      $("side-list").append(actions);
    }
    for (const item of annotations) {
      const prefix = { bookmark: translate("书签", "Bookmark"), note: translate("笔记", "Note"), highlight: translate("高亮", "Highlight") }[item.kind];
      const title = item.text || item.parts?.[0]?.quote || prefix;
      sideRow(prefix + " · " + (item.missing ? translate("位置已失效 · ", "Location lost · ") : "") + title, () => goAnnotation(item), [
        { label: translate("编辑", "Edit"), action: () => showEditor(item, true) }
      ]);
    }
  } else {
    if (!results.length) sideMessage(translate("没有匹配结果", "No matches"));
    for (const [index, result] of results.entries()) {
      sideRow(result.label, () => showSearchMatch(index));
    }
  }
}

async function showSearchMatch(index) {
  if (!results.length) {
    searchIndex = -1;
    $("search-count").textContent = "0 / 0";
    CSS.highlights?.delete("hayaku-search");
    return;
  }
  searchIndex = ((index % results.length) + results.length) % results.length;
  $("search-count").textContent = (searchIndex + 1) + " / " + results.length;
  const result = results[searchIndex];
  await jump(result.index, result);
  const range = locate(result);
  if (range && CSS.highlights) CSS.highlights.set("hayaku-search", new Highlight(range));
}

async function find(query) {
  query = query.trim();
  if (!book || !query) return;
  lastSearchQuery = query;
  const revision = ++searchRevision;
  const activeBook = book;
  const found = [];
  const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const expression = new RegExp(escaped, "giu");
  message(translate("正在搜索…", "Searching…"), 0);
  for (const element of units()) {
    const index = Number(element.dataset.unit);
    const text = pdf ? await pdf.text(index) : element.textContent;
    if (revision !== searchRevision || book !== activeBook) return;
    for (const match of text.matchAll(expression)) {
      const start = match.index;
      const quote = match[0];
      found.push({
        index, start, quote, before: "", after: "",
        label: (pdf ? "第 " + (index + 1) + " 页 · " : "") + text.slice(Math.max(0, start - 18), start + quote.length + 35)
      });
      if (found.length >= 300) break;
    }
    if (found.length >= 300) break;
  }
  results = found;
  searchIndex = -1;
  await showSearchMatch(0);
  if (!$("sidebar").hidden && currentTab === "search") await renderSidebar();
  message(found.length === 300 ? translate("显示前 300 条结果。", "Showing first 300 results.") : translate("找到 " + found.length + " 条结果。", "Found " + found.length + " matches."));
}

function settingsValues() {
  return {
    ...Object.fromEntries(["theme", "font-size", "line-height", "text-width"].map(id => [id, $(id).value])),
    "restore-session": $("restore-session").checked,
    "smart-invert": $("smart-invert").checked,
    "auto-update": $("auto-update").checked,
    "install-dismissed": installDismissed
  };
}

function applySettings(values) {
  for (const id of ["restore-session", "smart-invert", "auto-update"]) {
    if (values?.[id] != null) $(id).checked = Boolean(values[id]);
  }
  for (const id of ["theme", "font-size", "line-height", "text-width"]) {
    if (values?.[id] != null) $(id).value = values[id];
  }
  const root = document.documentElement;
  root.dataset.theme = $("theme").value;
  $("smart-invert-label").hidden = $("theme").value !== "dark";
  const invert = isSmartInvertActive();
  root.dataset.smartInvert = String(invert);
  root.style.setProperty("--size", $("font-size").value + "px");
  root.style.setProperty("--leading", $("line-height").value);
  root.style.setProperty("--measure", $("text-width").value + "px");
  $("font-size-val").textContent = $("font-size").value + "px";
  $("line-height-val").textContent = $("line-height").value;
  $("text-width-val").textContent = $("text-width").value + "px";
  if (invert !== lastSmartInvert) {
    lastSmartInvert = invert;
    pdf?.refreshTheme?.();
    if (book && !pdf) applyFlowColors();
  }
}

for (const id of ["open-file", "welcome-open"]) {
  $(id).addEventListener("click", () => {
    openDocument().then(opened => { if (opened) drainDropped().catch(report); });
  });
}
for (const tab of document.querySelectorAll("[data-tab]")) tab.addEventListener("click", () => {
  currentTab = tab.dataset.tab;
  renderSidebar().catch(report);
});
$("toggle-sidebar").addEventListener("click", () => {
  setSidebar($("sidebar").hidden);
  if (!$("sidebar").hidden) renderSidebar().catch(report);
});
$("settings-toggle").addEventListener("click", () => {
  $("settings").hidden = !$("settings").hidden;
  $("settings-toggle").setAttribute("aria-expanded", String(!$("settings").hidden));
});
$("search-toggle").addEventListener("click", () => {
  $("search-form").hidden = !$("search-form").hidden;
  if (!$("search-form").hidden) $("query").focus();
});
$("search-close").addEventListener("click", () => {
  $("search-form").hidden = true;
  results = [];
  searchIndex = -1;
  lastSearchQuery = "";
  $("search-count").textContent = "";
  CSS.highlights?.delete("hayaku-search");
});
$("search-prev").addEventListener("click", () => showSearchMatch(searchIndex - 1).catch(report));
$("search-next").addEventListener("click", () => showSearchMatch(searchIndex + 1).catch(report));
$("search-form").addEventListener("submit", event => {
  event.preventDefault();
  const query = $("query").value.trim();
  if (query && query === lastSearchQuery && results.length) {
    showSearchMatch(searchIndex + 1).catch(report);
  } else {
    find(query).catch(report);
  }
});
$("query").addEventListener("keydown", event => {
  if (event.key === "Enter" && event.shiftKey) {
    event.preventDefault();
    const query = $("query").value.trim();
    if (query && query === lastSearchQuery && results.length) {
      showSearchMatch(searchIndex - 1).catch(report);
    } else {
      find(query).catch(report);
    }
  }
});
$("bookmark").addEventListener("click", () => {
  const position = activePosition(true);
  showEditor({ kind: "bookmark", text: bookmarkTitle(position), position, digest: book.digest });
});
$("page-note").addEventListener("click", () => {
  showEditor({ kind: "note", position: activePosition(true), digest: book.digest });
});
$("selection-tools").addEventListener("pointerdown", event => event.preventDefault());
$("copy").addEventListener("click", () => {
  if (picked) navigator.clipboard.writeText(picked.text).then(() => message(translate("已复制选中文字。", "Copied to clipboard."))).catch(report);
});
for (const swatch of document.querySelectorAll("[data-highlight]")) {
  swatch.addEventListener("click", () => {
    if (picked) addAnnotation({ kind: "highlight", color: swatch.dataset.highlight, parts: picked.parts });
  });
}
$("selection-note").addEventListener("click", () => {
  if (picked) showEditor({ kind: "note", parts: picked.parts, color: "yellow" });
});
$("editor").addEventListener("close", () => {
  if (!editing || !book) return;
  const { annotation, existing } = editing;
  editing = null;
  const action = $("editor").returnValue;
  if (action === "save") {
    const change = { ...annotation, text: $("note-text").value.trim(), color: $("color").value };
    if (change.kind === "note" && !change.text) { message("笔记内容为空。"); return; }
    if (change.kind === "bookmark" && !change.text) change.text = bookmarkTitle(change.position);
    if (!existing) { addAnnotation(change); return; }
    Object.assign(annotation, change);
  } else if (action === "delete" && existing) {
    getState().annotations = getState().annotations.filter(item => item.id !== annotation.id);
  } else return;
  refreshHighlights();
  save();
  renderSidebar().catch(report);
});

for (const id of ["theme", "font-size", "line-height", "text-width"]) {
  $(id).addEventListener("input", () => applySettings());
  $(id).addEventListener("change", () => post("/api/settings", settingsValues()).catch(report));
}
for (const id of ["restore-session", "smart-invert", "auto-update"]) {
  $(id).addEventListener("change", () => {
    applySettings();
    post("/api/settings", settingsValues()).catch(report);
  });
}
$("view-mode").addEventListener("click", async () => {
  if (!book || !["markdown", "html"].includes(book.kind)) return;
  const position = activePosition();
  getState().viewMode = getState().viewMode === "code" ? "rendered" : "code";
  updateViewModeButton();
  contents = [];
  try {
    await loadFlow();
    await jump(position.index, null, "", position.fraction);
    refreshHighlights();
    updateProgress();
    await renderSidebar();
    save();
  } catch (error) { report(error); }
});
$("reset-settings").addEventListener("click", () => {
  applySettings({ "font-size": 19, "line-height": 1.8, "text-width": 760, "smart-invert": true });
  post("/api/settings", settingsValues()).catch(report);
  message(translate("已恢复默认阅读设置。", "Reading settings reset."));
});
document.addEventListener("click", event => {
  const settings = $("settings");
  if (!settings.hidden && !settings.contains(event.target) && !$("settings-toggle").contains(event.target)) {
    settings.hidden = true;
    $("settings-toggle").setAttribute("aria-expanded", "false");
  }
});
reader.addEventListener("click", event => {
  if (!book || event.target.closest("a[href]") || !getSelection().isCollapsed) return;
  const element = event.target.closest("#reader > [data-unit]");
  const root = element && textRoot(element);
  if (!root) return;
  let caretNode = null;
  let caretOffset = 0;
  if (document.caretRangeFromPoint) {
    const caret = document.caretRangeFromPoint(event.clientX, event.clientY);
    if (caret) {
      caretNode = caret.startContainer;
      caretOffset = caret.startOffset;
    }
  } else if (document.caretPositionFromPoint) {
    const pos = document.caretPositionFromPoint(event.clientX, event.clientY);
    if (pos) {
      caretNode = pos.offsetNode;
      caretOffset = pos.offset;
    }
  }
  if (!caretNode || !root.contains(caretNode)) return;
  const unitIndex = Number(element.dataset.unit);
  for (const annotation of getState().annotations) {
    for (const part of annotation.parts ?? []) {
      if (part.index !== unitIndex) continue;
      const range = locate(part);
      if (range && range.isPointInRange(caretNode, caretOffset)) {
        showEditor(annotation, true);
        return;
      }
    }
  }
});
let wheelSaveTimer;
scroll.addEventListener("wheel", event => {
  if (!event.ctrlKey || !book) return;
  event.preventDefault();
  if (pdf) {
    changeZoom(() => pdf.zoom(event.deltaY < 0 ? 1.12 : 1 / 1.12));
  } else {
    const slider = $("font-size");
    const next = Math.max(Number(slider.min), Math.min(Number(slider.max), Number(slider.value) + (event.deltaY < 0 ? 1 : -1)));
    if (next !== Number(slider.value)) {
      applySettings({ "font-size": next });
      clearTimeout(wheelSaveTimer);
      wheelSaveTimer = setTimeout(() => post("/api/settings", settingsValues()).catch(report), 300);
    }
  }
}, { passive: false });
document.addEventListener("keydown", event => {
  if (document.querySelector("dialog[open]")) return;
  const ctrl = event.ctrlKey || event.metaKey;
  const inInput = event.target.matches("input, textarea, select") || event.target.isContentEditable;
  if (ctrl && event.key.toLowerCase() === "o") {
    event.preventDefault();
    openDocument().then(opened => { if (opened) drainDropped().catch(report); });
    return;
  }
  if (ctrl && event.key.toLowerCase() === "f" && book) {
    event.preventDefault();
    $("search-form").hidden = false;
    $("query").focus();
    $("query").select();
    return;
  }
  if (ctrl && event.key.toLowerCase() === "b") {
    event.preventDefault();
    setSidebar($("sidebar").hidden);
    if (!$("sidebar").hidden) renderSidebar().catch(report);
    return;
  }
  if (ctrl && event.key.toLowerCase() === "w" && book && documentTabs.has(book.id)) {
    event.preventDefault();
    closeDocumentTab(documentTabs.get(book.id)).catch(report);
    return;
  }
  if (ctrl && event.key === "Tab" && documentTabs.size > 1) {
    event.preventDefault();
    const list = [...documentTabs.values()];
    const currentIndex = list.findIndex(entry => entry.id === book?.id);
    const nextIndex = (currentIndex + (event.shiftKey ? -1 : 1) + list.length) % list.length;
    openDocument({ id: list[nextIndex].id });
    return;
  }
  if (event.key === "F11" && book) {
    event.preventDefault();
    document.body.classList.toggle("focus-mode");
    return;
  }
  if (ctrl && (event.key === "+" || event.key === "=" || event.key === "-" || event.key === "0") && book) {
    event.preventDefault();
    if (pdf) {
      if (event.key === "0") changeZoom(() => pdf.fit());
      else changeZoom(() => pdf.zoom(event.key === "-" ? 1 / 1.2 : 1.2));
    } else {
      const next = event.key === "0" ? 19 : Math.max(14, Math.min(32, Number($("font-size").value) + (event.key === "-" ? -1 : 1)));
      applySettings({ "font-size": next });
      post("/api/settings", settingsValues()).catch(report);
    }
    return;
  }
  if (event.key === "Escape") {
    if (document.body.classList.contains("focus-mode")) {
      document.body.classList.remove("focus-mode");
    } else if (!$("settings").hidden) {
      $("settings").hidden = true;
      $("settings-toggle").setAttribute("aria-expanded", "false");
    } else if (!$("search-form").hidden) {
      $("search-close").click();
    } else if (!$("selection-tools").hidden) {
      $("selection-tools").hidden = true;
      getSelection()?.removeAllRanges();
    }
    return;
  }
  if (inInput || !book) return;
  if (event.key.toLowerCase() === "b") {
    event.preventDefault();
    $("bookmark").click();
  } else if (pdf && (event.key === "ArrowLeft" || event.key === "PageUp")) {
    event.preventDefault();
    jump(Math.max(0, activePosition().index - 1)).catch(report);
  } else if (pdf && (event.key === "ArrowRight" || event.key === "PageDown")) {
    event.preventDefault();
    jump(Math.min(pdf.count - 1, activePosition().index + 1)).catch(report);
  }
});
$("encoding").addEventListener("change", async () => {
  if (book?.kind !== "text") return;
  getState().encoding = $("encoding").value;
  try {
    const position = activePosition();
    await loadFlow();
    await jump(position.index, null, "", position.fraction);
    refreshHighlights();
    save();
  } catch (error) { report(error); }
});
$("page-number").addEventListener("change", () => {
  if (pdf) jump(Math.max(0, Math.min(pdf.count - 1, Number($("page-number").value) - 1))).catch(report);
});
$("previous").addEventListener("click", () => jump(Math.max(0, activePosition().index - 1)).catch(report));
$("next").addEventListener("click", () => jump(Math.min(pdf.count - 1, activePosition().index + 1)).catch(report));
$("zoom-out").addEventListener("click", () => changeZoom(() => pdf.zoom(1 / 1.2)));
$("zoom-in").addEventListener("click", () => changeZoom(() => pdf.zoom(1.2)));
$("fit").addEventListener("click", () => changeZoom(() => pdf.fit()));
let width = scroll.clientWidth;
let resizeTimer;
new ResizeObserver(() => {
  if (width === scroll.clientWidth) return;
  width = scroll.clientWidth;
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => changeZoom(() => pdf?.resize()), 150);
}).observe(scroll);

scroll.addEventListener("scroll", () => {
  if (progressFrame) return;
  progressFrame = requestAnimationFrame(() => { progressFrame = null; updateProgress(); });
}, { passive: true });
document.addEventListener("selectionchange", selectionToolbar);
document.addEventListener("copy", event => {
  if (book?.kind !== "word" || !event.clipboardData) return;
  const parts = selectionParts(getSelection());
  if (!parts.length) return;
  event.preventDefault();
  event.clipboardData.setData("text/plain", parts.map(part => part.quote).join("\n"));
});
document.addEventListener("visibilitychange", () => { if (document.hidden && saveTimer) save(true); });
window.addEventListener("pagehide", () => { if (saveTimer) save(true); });
window.addEventListener("dragover", event => {
  if (!event.dataTransfer.types.includes("Files")) return;
  event.preventDefault();
  document.body.classList.add("dragging");
});
window.addEventListener("dragleave", event => { if (!event.relatedTarget) document.body.classList.remove("dragging"); });
window.addEventListener("drop", async event => {
  event.preventDefault();
  document.body.classList.remove("dragging");
  for (const file of Array.from(event.dataTransfer.files)) {
    await openDocument({}, file);
  }
});

function localizeStaticUi() {
  if (!english) return;
  const text = (selector, value) => {
    const element = document.querySelector(selector);
    if (element) element.textContent = value;
  };
  const labelText = (controlId, value) => {
    const label = $(controlId)?.closest("label");
    if (!label) return;
    for (const node of label.childNodes) {
      if (node.nodeType === Node.TEXT_NODE && node.textContent.trim()) {
        node.textContent = value;
        break;
      }
    }
  };
  text('[data-tab="recent"]', "Recent");
  text('[data-tab="contents"]', "Contents");
  text('[data-tab="notes"]', "Annotations");
  $("toggle-sidebar").setAttribute("aria-label", "Toggle sidebar");
  $("toggle-sidebar").title = "Toggle sidebar";
  $("open-file").textContent = "Open file";
  $("welcome-open").textContent = "Open file";
  $("search-toggle").textContent = "Search";
  $("bookmark").textContent = "Bookmark";
  $("page-note").textContent = "Note";
  $("settings-toggle").textContent = "Settings";
  $("query").placeholder = "Search text";
  $("query").setAttribute("aria-label", "Search text");
  text('#search-form button[type="submit"]', "Find");
  $("search-prev").setAttribute("aria-label", "Previous match");
  $("search-next").setAttribute("aria-label", "Next match");
  $("search-close").textContent = "Close";
  $("previous").setAttribute("aria-label", "Previous page");
  $("page-number").setAttribute("aria-label", "Go to page");
  $("next").setAttribute("aria-label", "Next page");
  $("zoom-out").setAttribute("aria-label", "Zoom out");
  $("fit").textContent = "Fit width";
  $("zoom-in").setAttribute("aria-label", "Zoom in");
  $("copy").textContent = "Copy";
  document.querySelector('[data-highlight="yellow"]')?.setAttribute("aria-label", "Yellow highlight");
  document.querySelector('[data-highlight="green"]')?.setAttribute("aria-label", "Green highlight");
  document.querySelector('[data-highlight="blue"]')?.setAttribute("aria-label", "Blue highlight");
  document.querySelector('[data-highlight="pink"]')?.setAttribute("aria-label", "Pink highlight");
  for (const swatch of document.querySelectorAll("[data-highlight]")) swatch.title = swatch.getAttribute("aria-label");
  $("selection-note").textContent = "Note";
  $("delete-note").textContent = "Delete";
  text('#editor button[value="cancel"]', "Cancel");
  text('#editor button[value="save"]', "Save");
  $("check-updates").textContent = "Check for updates";
  $("reset-settings").textContent = "Reset defaults";
  $("welcome-recent-title").textContent = "Recent";
  $("welcome-clear-recent").textContent = "Clear history";
  labelText("theme", "Theme ");
  text('#theme option[value="light"]', "Light");
  text('#theme option[value="dark"]', "Dark");
  labelText("font-size", "Font size ");
  labelText("line-height", "Line height ");
  labelText("text-width", "Text width ");
  labelText("encoding", "TXT encoding ");
  text('#encoding option[value=""]', "Auto");
  labelText("restore-session", "Restore tabs on startup");
  labelText("smart-invert", "Smart page inversion");
  labelText("auto-update", "Install updates automatically");
  updateViewModeButton();
  labelText("note-text", "Note ");
  labelText("color", "Color ");
  text('#color option[value="yellow"]', "Yellow");
  text('#color option[value="green"]', "Green");
  text('#color option[value="blue"]', "Blue");
  text('#color option[value="pink"]', "Pink");
}

function hideSplash() {
  const splash = $("splash");
  if (splash.hidden || splash.classList.contains("fading")) return;
  splash.classList.add("fading");
  setTimeout(() => { splash.hidden = true; }, 220);
}

function renderIntro(intro) {
  $("project-intro").replaceChildren(purify.sanitize(intro, {
    RETURN_DOM_FRAGMENT: true,
    ALLOWED_TAGS: ["h1", "h2", "p", "ul", "ol", "li", "strong", "em", "code", "a"],
    ALLOWED_ATTR: ["href"]
  }));
  for (const link of $("project-intro").querySelectorAll("a[href]")) {
    const href = link.getAttribute("href") || "";
    if (href === "https://github.com/drliuhuan/HayakuReader" || href.startsWith("https://github.com/drliuhuan/HayakuReader/")) {
      if (isDesktop) {
        link.addEventListener("click", event => {
          event.preventDefault();
          post("/api/project/open", {}).catch(report);
        });
      } else {
        link.target = "_blank";
        link.rel = "noopener noreferrer";
      }
    } else {
      link.removeAttribute("href");
    }
  }
}

$("welcome-clear-recent").addEventListener("click", () => clearRecentHistory().catch(report));

async function start() {
  if (!key) { message("请从 HayakuReader 启动阅读窗口。"); return; }
  document.documentElement.lang = english ? "en" : "zh-CN";
  localizeStaticUi();
  const hasOpen = new URLSearchParams(location.search).has("open");
  const language = english ? "en" : "zh";
  const project = isDesktop
    ? request("/api/project?lang=" + language).then(response => response.json()).then(value => value.html).catch(() => "")
    : Promise.resolve("");
  const local = request("/api/project/local?lang=" + language).then(response => response.text()).catch(() => "");
  const release = isDesktop ? request("/api/release").then(response => response.json()).catch(() => null) : Promise.resolve(null);
  const preferences = request("/api/settings").then(response => response.json());
  const workspace = request("/api/workspace").then(response => response.json());
  const installState = isDesktop ? request("/api/installation").then(response => response.json()) : Promise.resolve(null);

  let skipSplash = () => {};
  const splash = new Promise(resolve => {
    const timer = setTimeout(resolve, isDesktop && !hasOpen ? 3000 : 0);
    skipSplash = () => { clearTimeout(timer); resolve(); };
  });
  $("splash").addEventListener("click", skipSplash, { once: true });
  document.addEventListener("keydown", event => {
    if (event.key === "Escape" && !$("splash").hidden) skipSplash();
  }, { once: true });

  if (hasOpen) {
    const settings = await preferences;
    applySettings(settings);
    installDismissed = Boolean(settings?.["install-dismissed"]);
    installation = await installState;
    if (installation) {
      $("association-settings").hidden = installation.installationExists && !installation.installed;
      $("association-settings").textContent = installation.installed
        ? translate("文件关联", "File associations") : translate("安装", "Install");
      $("auto-update-label").hidden = !installation.installed;
      $("check-updates").hidden = false;
    }
    const saved = await workspace;
    if ($("restore-session").checked && saved?.tabs) {
      for (const entry of saved.tabs) documentTabs.set(entry.id, { ...entry, document: null });
      renderDocumentTabs();
    }
    try {
      await openDocument({ initial: true });
    } finally {
      hideSplash();
    }
    Promise.race([project, new Promise(resolve => setTimeout(() => resolve(""), 2500))])
      .then(async remote => renderIntro(remote || await local))
      .catch(() => {});
    renderWelcomeRecent().catch(() => {});
    await renderSidebar();
  } else {
    const remoteIntro = await Promise.race([project, splash.then(() => "")]);
    const intro = remoteIntro || await local;
    renderIntro(intro);
    await splash;
    hideSplash();
    const settings = await preferences;
    applySettings(settings);
    installDismissed = Boolean(settings?.["install-dismissed"]);
    installation = await installState;
    if (installation) {
      $("association-settings").hidden = installation.installationExists && !installation.installed;
      $("association-settings").textContent = installation.installed
        ? translate("文件关联", "File associations") : translate("安装", "Install");
      $("auto-update-label").hidden = !installation.installed;
      $("check-updates").hidden = false;
    }
    const saved = await workspace;
    if ($("restore-session").checked && saved?.tabs) {
      for (const entry of saved.tabs) documentTabs.set(entry.id, { ...entry, document: null });
      renderDocumentTabs();
    }
    renderWelcomeRecent().catch(() => {});
    await renderSidebar();
    if ($("restore-session").checked && saved?.active && documentTabs.has(saved.active)) {
      await openDocument({ id: saved.active });
    }
  }
  started = true;
  if (installation && !installation.installationExists && !installDismissed) showInstallation();
  else {
    release.then(info => showRelease(info)).catch(report);
    drainDropped().catch(report);
  }
}

function showInstallation() {
  if (!installation || $("installation").open) return;
  $("installation-title").textContent = installation.installed
    ? translate("文件关联", "File associations") : translate("安装 HayakuReader", "Install HayakuReader");
  $("installation-path").textContent = installation.path;
  $("association-legend").textContent = translate("关联的文件格式", "File types to associate");
  $("association-hint").textContent = translate(
    "Windows 已设置过默认应用的格式，仍由系统默认应用设置决定。",
    "For file types with an existing Windows default, choose HayakuReader in Windows Default apps."
  );
  $("installation-cancel").textContent = translate("取消", "Cancel");
  $("installation-apply").textContent = translate("确定", "Apply");
  $("windows-defaults").hidden = !installation.installed;
  $("windows-defaults").textContent = translate("打开 Windows 默认应用", "Open Windows Default apps");
  const list = $("association-list");
  list.replaceChildren();
  for (const extension of installation.extensions) {
    const label = document.createElement("label");
    const box = document.createElement("input");
    box.type = "checkbox";
    box.value = extension;
    box.checked = installation.associations.includes(extension);
    label.append(box, "." + extension);
    list.append(label);
  }
  $("installation").returnValue = "cancel";
  $("installation").showModal();
}

async function restart() {
  await Promise.all([save(false).catch(() => {}), persistWorkspace(false)]);
  window.ipc?.postMessage("restart");
}

$("association-settings").onclick = showInstallation;
$("windows-defaults").onclick = () => post("/api/installation/defaults", {}).catch(report);
$("installation").addEventListener("close", async () => {
  if ($("installation").returnValue !== "apply") {
    if (!installation.installed) {
      installDismissed = true;
      post("/api/settings", settingsValues()).catch(report);
    }
    return;
  }
  const extensions = [...$("association-list").querySelectorAll("input:checked")].map(box => box.value);
  try {
    const result = await (await post("/api/installation", { extensions, install: !installation.installed })).json();
    installation.associations = extensions;
    if (result.restart) await restart();
    else message(translate("已保存文件关联。", "File associations saved."));
  } catch (error) { report(error); }
});

async function installUpdate() {
  if (updateInProgress) return;
  updateInProgress = true;
  $("update-transfer").hidden = false;
  $("update-progress").hidden = false;
  $("update-progress").value = 0;
  $("update-speed").textContent = "0% · 0 B/s";
  $("manual-download").hidden = true;
  message(translate("正在下载并验证更新…", "Downloading and verifying update…"), 0);
  const poll = setInterval(async () => {
    try {
      const progress = await (await request("/api/release/progress")).json();
      if (!progress || !updateInProgress) return;
      const percent = progress.total ? Math.min(100, 100 * progress.downloaded / progress.total) : 0;
      $("update-progress").value = percent;
      const speed = progress.bytes_per_second;
      const unit = speed >= 1048576 ? [1048576, "MiB/s"] : speed >= 1024 ? [1024, "KiB/s"] : [1, "B/s"];
      $("update-speed").textContent = Math.floor(percent) + "% · " + (speed / unit[0]).toFixed(unit[0] === 1 ? 0 : 1) + " " + unit[1];
    } catch { /* The download request reports any failure. */ }
  }, 250);
  try {
    const result = await (await post("/api/release", {})).json();
    if (result.restart) {
      $("update-progress").value = 100;
      $("update-speed").textContent = "100%";
      message(translate("更新已下载，正在重新启动…", "Update verified. Restarting…"));
      await restart();
    }
  } catch (error) { showUpdateFailure(error); }
  finally { clearInterval(poll); updateInProgress = false; }
}

function showUpdateFailure(error) {
  message(translate("更新失败：", "Update failed: ") + error.message, 0);
  $("update-transfer").hidden = false;
  $("update-progress").hidden = true;
  $("update-speed").textContent = "";
  $("manual-download").hidden = false;
  $("manual-download").textContent = translate("前往 GitHub Release 手动下载", "Download manually from GitHub Releases");
}

$("manual-download").onclick = event => {
  event.preventDefault();
  post("/api/release/open", {}).catch(showUpdateFailure);
};

function showRelease(info, manual = false) {
  if (!info?.version) {
    if (manual) message(translate("已是最新版本。", "HayakuReader is up to date."));
    return;
  }
  if (info.installed && $("auto-update").checked && !manual) {
    installUpdate();
    return;
  }
  if (!info.installed) {
    message(translate("发现更新 " + info.version + "。安装后可在应用内更新。", "Update " + info.version + " is available. Install the app to update in place."));
    return;
  }
  if ($("update-dialog").open) return;
  $("update-title").textContent = translate("发现新版本 ", "Update available ") + info.version;
  $("update-description").textContent = translate("更新后会重新启动 HayakuReader。", "HayakuReader will restart after the update.");
  $("update-apply").textContent = translate("立即更新", "Update now");
  $("update-cancel").textContent = translate("稍后", "Later");
  $("update-dialog").returnValue = "cancel";
  $("update-dialog").showModal();
}

$("check-updates").onclick = () => request("/api/release").then(response => response.json())
  .then(info => showRelease(info, true)).catch(showUpdateFailure);
$("update-dialog").addEventListener("close", () => {
  if ($("update-dialog").returnValue === "update") installUpdate();
});

async function drainDropped() {
  if (drainingDrops) return;
  drainingDrops = true;
  try {
    while (started && !openBusy && !$("installation").open) {
      if (!await openDocument({ dropped: true })) break;
    }
  } finally {
    drainingDrops = false;
  }
}

window.hayakuOpenDrop = () => {
  drainDropped().catch(report);
};
window.hayakuFlush = async () => {
  try { await Promise.all([save(false), persistWorkspace(false)]); }
  finally { window.ipc?.postMessage("saved"); }
};

start().catch(report);
