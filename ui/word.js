import { applySmartInvertToCanvas } from "/ui/theme-color.js";

export function openWord(metadata, contentRoute, request, reader, scroll, onRender, shouldSmartInvert = () => false) {
  const firstWidth = Math.max(1, metadata.pages[0]?.[0] || 1);
  const entries = [];
  const measure = document.createElement("canvas").getContext("2d");
  let zoom = null;
  let closed = false;
  let useTick = 0;
  const MAX_RENDERED_PAGES = 10;
  const observer = new IntersectionObserver(records => {
    for (const record of records) {
      const index = Number(record.target.dataset.unit);
      const entry = entries[index];
      if (!entry) continue;
      entry.visible = record.isIntersecting;
      if (record.isIntersecting) {
        entry.lastUsed = ++useTick;
        render(index).catch(console.error);
      }
    }
  }, { root: scroll, rootMargin: "480px" });

  function evictPages() {
    const totalRendered = entries.filter(entry => entry.pending).length;
    if (totalRendered <= MAX_RENDERED_PAGES) return;
    const candidates = entries.filter(entry => entry.pending && !entry.visible);
    candidates.sort((a, b) => a.lastUsed - b.lastUsed);
    let excess = totalRendered - MAX_RENDERED_PAGES;
    for (const entry of candidates) {
      if (excess <= 0) break;
      entry.bitmap?.close();
      entry.bitmap = null;
      if (entry.canvas) {
        entry.canvas.width = 0;
        entry.canvas.height = 0;
        entry.canvas = null;
      }
      entry.layer = null;
      entry.pending = null;
      entry.element.replaceChildren();
      excess--;
    }
  }

  function scale() {
    return zoom ?? Math.max(.2, (scroll.clientWidth - 36) / firstWidth);
  }

  function resize() {
    const factor = scale();
    for (const [index, [width, height]] of metadata.pages.entries()) {
      const entry = entries[index];
      entry.element.style.width = width * factor + "px";
      entry.element.style.height = height * factor + "px";
      if (entry.layer) entry.layer.style.transform = "scale(" + factor + ")";
    }
  }

  function getText(index) {
    const entry = entries[index];
    if (!entry.text) {
      entry.text = request(contentRoute(index)).then(response => response.json()).catch(error => {
        entry.text = null;
        throw error;
      });
    }
    return entry.text;
  }

  function paintCanvas(entry, index) {
    if (!entry.bitmap || !entry.canvas) return;
    const ctx = entry.canvas.getContext("2d", { willReadFrequently: true });
    ctx.clearRect(0, 0, entry.canvas.width, entry.canvas.height);
    ctx.drawImage(entry.bitmap, 0, 0);
    if (shouldSmartInvert()) {
      const [pw, ph] = metadata.pages[index];
      const sx = entry.canvas.width / Math.max(1, pw);
      const sy = entry.canvas.height / Math.max(1, ph);
      const pageArea = Math.max(1, pw * ph);
      const boxes = metadata.images?.[index] ?? [];
      const hasFullPage = boxes.some(([, , w, h]) => w * h >= pageArea * 0.75);
      const preserveRects = hasFullPage
        ? []
        : boxes
            .filter(([, , w, h]) => w >= 8 && h >= 8)
            .map(([x, y, w, h]) => ({ x: x * sx, y: y * sy, width: w * sx, height: h * sy }));
      applySmartInvertToCanvas(entry.canvas, preserveRects);
    }
  }

  function buildTextLayer(index, runs) {
    const [width, height] = metadata.pages[index];
    const layer = document.createElement("div");
    layer.className = "word-text";
    layer.style.width = width + "px";
    layer.style.height = height + "px";
    layer.style.transform = "scale(" + scale() + ")";
    for (const run of runs) {
      if (run.line_break) layer.append(document.createTextNode("\n"));
      const span = document.createElement("span");
      span.textContent = run.text;
      span.style.left = run.left + "px";
      span.style.top = run.top + "px";
      span.style.fontSize = run.size + "px";
      measure.font = run.size + "px sans-serif";
      const measured = measure.measureText(run.text).width;
      if (measured > 0) span.style.transform = "scaleX(" + run.width / measured + ")";
      layer.append(span);
    }
    return layer;
  }

  function render(index) {
    const entry = entries[index];
    if (!entry || closed) return Promise.resolve();
    entry.lastUsed = ++useTick;
    if (!entry.pending) entry.pending = (async () => {
      const [runs, response] = await Promise.all([getText(index), request(contentRoute(index) + "&raster=true")]);
      const blob = await response.blob();
      if (closed) return;
      const bitmap = await createImageBitmap(blob);
      if (closed) {
        bitmap.close();
        return;
      }
      const canvas = document.createElement("canvas");
      canvas.width = bitmap.width;
      canvas.height = bitmap.height;
      entry.bitmap = bitmap;
      entry.canvas = canvas;
      paintCanvas(entry, index);
      const layer = buildTextLayer(index, runs);
      entry.layer = layer;
      entry.element.replaceChildren(canvas, layer);
      evictPages();
      onRender();
    })().catch(error => {
      entry.pending = null;
      if (!closed) throw error;
    });
    return entry.pending;
  }

  for (let index = 0; index < metadata.pages.length; index++) {
    const element = document.createElement("section");
    element.className = "pdf-page word-page";
    element.dataset.unit = index;
    element.setAttribute("aria-label", "第 " + (index + 1) + " 页");
    reader.append(element);
    entries.push({ element, pending: null, bitmap: null, canvas: null, layer: null, visible: false, lastUsed: 0 });
    observer.observe(element);
  }
  resize();

  return {
    render, resize,
    count: metadata.pages.length,
    fit() { zoom = null; resize(); },
    zoom(by) { zoom = Math.max(.25, Math.min(5, scale() * by)); resize(); },
    refreshTheme() {
      for (const [index, entry] of entries.entries()) {
        if (entry.bitmap && entry.canvas) paintCanvas(entry, index);
      }
    },
    missingTextCount() {
      return entries.filter(entry => !entry.layer).length;
    },
    async ensureAllTextLayers(onProgress, signal) {
      const missing = [];
      for (let i = 0; i < entries.length; i++) {
        if (!entries[i].layer) missing.push(i);
      }
      const total = missing.length;
      if (!total) return;
      let cursor = 0;
      let done = 0;
      await Promise.all(Array.from({ length: Math.min(4, total) }, async () => {
        while (cursor < missing.length && !signal?.aborted && !closed) {
          const index = missing[cursor++];
          const entry = entries[index];
          if (!entry || entry.layer) {
            onProgress?.(++done, total);
            continue;
          }
          const runs = await getText(index);
          if (signal?.aborted || closed) return;
          if (!entry.layer) {
            const layer = buildTextLayer(index, runs);
            entry.layer = layer;
            entry.element.append(layer);
          }
          onProgress?.(++done, total);
        }
      }));
    },
    async text(index) {
      const runs = await getText(index);
      return runs.map(run => (run.line_break ? "\n" : "") + run.text).join("");
    },
    close() {
      closed = true;
      observer.disconnect();
      for (const entry of entries) {
        entry.bitmap?.close();
        entry.bitmap = null;
        if (entry.canvas) {
          entry.canvas.width = 0;
          entry.canvas.height = 0;
          entry.canvas = null;
        }
      }
    }
  };
}
