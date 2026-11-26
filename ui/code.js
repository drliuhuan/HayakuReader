function token(text, cls) {
  if (!text) return null;
  if (!cls) return document.createTextNode(text);
  const span = document.createElement("span");
  span.className = cls;
  span.textContent = text;
  return span;
}

function appendTokens(container, tokens) {
  for (const [text, cls] of tokens) {
    const node = token(text, cls);
    if (node) container.append(node);
  }
}

function tokenizeHtml(source) {
  const tokens = [];
  const pattern = /(<!--[\s\S]*?-->)|(<!DOCTYPE[^>]*>)|(<\/?)([a-zA-Z][\w:-]*)([\s\S]*?)(\/?>)|(&(?:#\d+|#x[\da-fA-F]+|[a-zA-Z][\w-]+);)/gi;
  let last = 0;
  for (const match of source.matchAll(pattern)) {
    if (match.index > last) tokens.push([source.slice(last, match.index), null]);
    if (match[1]) {
      tokens.push([match[1], "tok-comment"]);
    } else if (match[2]) {
      tokens.push([match[2], "tok-meta"]);
    } else if (match[3]) {
      tokens.push([match[3], "tok-punct"]);
      tokens.push([match[4], "tok-tag"]);
      if (match[5]) tokenizeHtmlAttrs(match[5], tokens);
      tokens.push([match[6], "tok-punct"]);
    } else if (match[7]) {
      tokens.push([match[7], "tok-entity"]);
    }
    last = match.index + match[0].length;
  }
  if (last < source.length) tokens.push([source.slice(last), null]);
  return tokens;
}

function tokenizeHtmlAttrs(attrs, tokens) {
  const pattern = /([^\s=/>]+)(\s*=\s*)("[^"]*"|'[^']*'|[^\s"'=<>`]+)|([^\s=/>]+)/g;
  let last = 0;
  for (const match of attrs.matchAll(pattern)) {
    if (match.index > last) tokens.push([attrs.slice(last, match.index), null]);
    if (match[1]) {
      tokens.push([match[1], "tok-attr"]);
      tokens.push([match[2], "tok-punct"]);
      tokens.push([match[3], "tok-string"]);
    } else if (match[4]) {
      tokens.push([match[4], "tok-attr"]);
    }
    last = match.index + match[0].length;
  }
  if (last < attrs.length) tokens.push([attrs.slice(last), null]);
}

function tokenizeMarkdownInline(line) {
  const tokens = [];
  const pattern = /(`[^`]+`)|(!?\[[^\]]*\])(\([^)]*\))|(\*\*[^*]+\*\*|__[^_]+__)|(\*[^*\n]+\*|_[^_\n]+_)|(~~[^~]+~~)|(<\/?[a-zA-Z][\w:-]*(?:\s+[^>]*?)?\/?>)/g;
  let last = 0;
  for (const match of line.matchAll(pattern)) {
    if (match.index > last) tokens.push([line.slice(last, match.index), null]);
    if (match[1]) tokens.push([match[1], "tok-code"]);
    else if (match[2]) {
      tokens.push([match[2], "tok-tag"]);
      tokens.push([match[3], "tok-string"]);
    } else if (match[4]) tokens.push([match[4], "tok-strong"]);
    else if (match[5]) tokens.push([match[5], "tok-em"]);
    else if (match[6]) tokens.push([match[6], "tok-comment"]);
    else if (match[7]) tokens.push([match[7], "tok-tag"]);
    last = match.index + match[0].length;
  }
  if (last < line.length) tokens.push([line.slice(last), null]);
  return tokens;
}

function splitTokensByLine(tokens) {
  const lines = [[]];
  for (const [text, cls] of tokens) {
    const parts = text.split("\n");
    for (let i = 0; i < parts.length; i++) {
      if (parts[i]) lines[lines.length - 1].push([parts[i], cls]);
      if (i < parts.length - 1) lines.push([]);
    }
  }
  return lines;
}

function tokenizeMarkdownLines(source) {
  const rawLines = source.split("\n");
  const lines = [];
  let fenceChar = "";
  let fenceLen = 0;

  for (const line of rawLines) {
    const fenceMatch = line.match(/^\s{0,3}(`{3,}|~{3,})(.*)$/);
    if (!fenceChar && fenceMatch) {
      fenceChar = fenceMatch[1][0];
      fenceLen = fenceMatch[1].length;
      lines.push([[line, "tok-meta"]]);
      continue;
    }
    if (fenceChar) {
      lines.push([[line, fenceMatch && fenceMatch[1][0] === fenceChar && fenceMatch[1].length >= fenceLen && !fenceMatch[2].trim() ? "tok-meta" : "tok-code"]]);
      if (fenceMatch && fenceMatch[1][0] === fenceChar && fenceMatch[1].length >= fenceLen && !fenceMatch[2].trim()) {
        fenceChar = "";
        fenceLen = 0;
      }
      continue;
    }
    if (/^\s{0,3}#{1,6}(?:\s|$)/.test(line)) {
      lines.push([[line, "tok-heading"]]);
      continue;
    }
    if (/^\s{0,3}(?:[-*_]\s*){3,}$/.test(line)) {
      lines.push([[line, "tok-punct"]]);
      continue;
    }
    const quoteMatch = line.match(/^(\s{0,3}>+\s?)(.*)$/);
    if (quoteMatch) {
      lines.push([[quoteMatch[1], "tok-punct"], [quoteMatch[2], "tok-quote"]]);
      continue;
    }
    const listMatch = line.match(/^(\s*(?:[-*+]|\d+\.)\s+(?:\[[ xX]\]\s+)?)(.*)$/);
    if (listMatch) {
      lines.push([[listMatch[1], "tok-keyword"], ...tokenizeMarkdownInline(listMatch[2])]);
      continue;
    }
    lines.push(tokenizeMarkdownInline(line));
  }
  return lines;
}

export function renderCodeView(source, kind) {
  const normalized = source.replace(/\r\n?/g, "\n");
  const lineTokens = kind === "html"
    ? splitTokensByLine(tokenizeHtml(normalized))
    : tokenizeMarkdownLines(normalized);

  const root = document.createElement("div");
  root.className = "code-view";
  root.dataset.unit = "0";
  const digits = String(lineTokens.length).length;
  root.style.setProperty("--gutter-ch", Math.max(3, digits + 1) + "ch");

  const fragment = document.createDocumentFragment();
  for (let i = 0; i < lineTokens.length; i++) {
    const row = document.createElement("div");
    row.className = "code-line";
    appendTokens(row, lineTokens[i]);
    if (i < lineTokens.length - 1) {
      const nl = document.createElement("span");
      nl.className = "code-nl";
      nl.textContent = "\n";
      row.append(nl);
    }
    fragment.append(row);
  }
  root.append(fragment);
  return root;
}
