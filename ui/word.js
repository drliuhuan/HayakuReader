import { applySmartInvertToCanvas } from "/ui/theme-color.js";

const BARE_URL_RE = /https?:\/\/[^\s<>"'`()[\]{}，。、；：！？（）【】《》]+/;

function stripUrlTrailingPunct(url) {
  return url.replace(/[.,;:!?]+$/, "");
}

function canvasToPngBlob(canvas) {
  return new Promise((resolve, reject) => {
    canvas.toBlob(blob => {
      if (blob) resolve(blob);
      else reject(new Error("无法导出图像"));
    }, "image/png");
  });
}

export function openWord(metadata, contentRoute, request, reader, scroll, onRender, shouldSmartInvert = () => false, onExternalLink = () => {}) {
  const firstWidth = Math.max(1, metadata.pages[0]?.[0] || 1);
  const entries = [];
  const measure = document.createElement("canvas").getContext("2d");
  let zoom = null;
  let closed = false;
  let useTick = 0;
  let rasterRefreshTimer;
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
      entry.renderedScale = 0;
      entry.element.replaceChildren();
      excess--;
    }
  }

  function scale() {
    return zoom ?? Math.max(.2, (scroll.clientWidth - 36) / firstWidth);
  }

  function targetRenderScale(index) {
    const [pw, ph] = metadata.pages[index];
    const factor = scale();
    const dpr = Math.max(window.devicePixelRatio || 1, 1.5);
    const maxScale = Math.min(6, Math.sqrt(16000000 / Math.max(1, pw * ph)));
    return Math.round(Math.min(maxScale, Math.max(1.5, factor * dpr)) * 10) / 10;
  }

  async function renderRaster(index, desiredScale) {
    const entry = entries[index];
    if (!entry || closed) return;
    const token = ++entry.rasterToken;
    const response = await request(contentRoute(index) + "&raster=true&scale=" + desiredScale);
    const blob = await response.blob();
    if (closed || token !== entry.rasterToken) return;
    const bitmap = await createImageBitmap(blob);
    if (closed || token !== entry.rasterToken) {
      bitmap.close();
      return;
    }
    entry.bitmap?.close();
    entry.bitmap = bitmap;
    if (!entry.canvas) {
      entry.canvas = document.createElement("canvas");
      entry.element.prepend(entry.canvas);
    }
    entry.canvas.width = bitmap.width;
    entry.canvas.height = bitmap.height;
    entry.renderedScale = desiredScale;
    paintCanvas(entry, index);
    onRender();
  }

  function refreshRasters() {
    for (const [index, entry] of entries.entries()) {
      if (!entry.pending || !entry.canvas) continue;
      const desired = targetRenderScale(index);
      const ratio = entry.renderedScale ? desired / entry.renderedScale : 0;
      if (ratio >= 0.88 && ratio <= 1.12) continue;
      if (entry.visible) {
        renderRaster(index, desired).catch(console.error);
      } else {
        entry.bitmap?.close();
        entry.bitmap = null;
        entry.canvas.width = 0;
        entry.canvas.height = 0;
        entry.canvas = null;
        entry.layer = null;
        entry.pending = null;
        entry.renderedScale = 0;
        entry.element.replaceChildren();
      }
    }
  }

  function resize() {
    const factor = scale();
    for (const [index, [width, height]] of metadata.pages.entries()) {
      const entry = entries[index];
      entry.element.style.width = width * factor + "px";
      entry.element.style.height = height * factor + "px";
      if (entry.layer) entry.layer.style.transform = "scale(" + factor + ")";
    }
    clearTimeout(rasterRefreshTimer);
    rasterRefreshTimer = setTimeout(refreshRasters, 150);
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
      let targetUrl = run.url || "";
      if (!targetUrl) {
        const match = BARE_URL_RE.exec(run.text || "");
        if (match) targetUrl = stripUrlTrailingPunct(match[0]);
      }
      if (targetUrl) {
        span.classList.add("word-link");
        span.title = targetUrl;
        span.addEventListener("click", event => {
          if (window.getSelection()?.toString()) return;
          event.preventDefault();
          onExternalLink(targetUrl);
        });
      }
      layer.append(span);
    }
    return layer;
  }

  function render(index) {
    const entry = entries[index];
    if (!entry || closed) return Promise.resolve();
    entry.lastUsed = ++useTick;
    if (!entry.pending) entry.pending = (async () => {
      const desiredScale = targetRenderScale(index);
      const token = ++entry.rasterToken;
      const [runs, response] = await Promise.all([
        getText(index),
        request(contentRoute(index) + "&raster=true&scale=" + desiredScale)
      ]);
      const blob = await response.blob();
      if (closed || token !== entry.rasterToken) return;
      const bitmap = await createImageBitmap(blob);
      if (closed || token !== entry.rasterToken) {
        bitmap.close();
        return;
      }
      const canvas = document.createElement("canvas");
      canvas.width = bitmap.width;
      canvas.height = bitmap.height;
      entry.bitmap?.close();
      entry.bitmap = bitmap;
      entry.canvas = canvas;
      entry.renderedScale = desiredScale;
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
    entries.push({ element, pending: null, bitmap: null, canvas: null, layer: null, renderedScale: 0, rasterToken: 0, visible: false, lastUsed: 0 });
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
    imageAt(pageIndex, clientX, clientY) {
      const entry = entries[pageIndex];
      if (!entry?.bitmap) return null;
      const [pw, ph] = metadata.pages[pageIndex];
      const box = entry.element.getBoundingClientRect();
      const factor = scale();
      const px = (clientX - box.left) / factor;
      const py = (clientY - box.top) / factor;
      let matched = null;
      let matchedArea = Infinity;
      for (const [x, y, w, h] of metadata.images?.[pageIndex] ?? []) {
        if (w >= 8 && h >= 8 && px >= x && px <= x + w && py >= y && py <= y + h) {
          const area = w * h;
          if (area <= matchedArea) {
            matched = [x, y, w, h];
            matchedArea = area;
          }
        }
      }
      if (matched) {
        const [mx, my, mw, mh] = matched;
        const sxScale = entry.bitmap.width / Math.max(1, pw);
        const syScale = entry.bitmap.height / Math.max(1, ph);
        const sx = Math.max(0, Math.floor(mx * sxScale));
        const sy = Math.max(0, Math.floor(my * syScale));
        const sw = Math.max(1, Math.min(entry.bitmap.width - sx, Math.ceil(mw * sxScale)));
        const sh = Math.max(1, Math.min(entry.bitmap.height - sy, Math.ceil(mh * syScale)));
        return {
          kind: "image",
          filename: "page-" + (pageIndex + 1) + "-image.png",
          extractPng: () => {
            const out = document.createElement("canvas");
            out.width = sw;
            out.height = sh;
            out.getContext("2d").drawImage(entry.bitmap, sx, sy, sw, sh, 0, 0, sw, sh);
            return canvasToPngBlob(out);
          }
        };
      }
      return {
        kind: "page",
        filename: "page-" + (pageIndex + 1) + ".png",
        extractPng: () => {
          const out = document.createElement("canvas");
          out.width = entry.bitmap.width;
          out.height = entry.bitmap.height;
          out.getContext("2d").drawImage(entry.bitmap, 0, 0);
          return canvasToPngBlob(out);
        }
      };
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
      clearTimeout(rasterRefreshTimer);
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
