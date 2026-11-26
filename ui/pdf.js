import * as pdfjs from "/vendor/pdf.mjs";
import { applySmartInvertToCanvas } from "/ui/theme-color.js";

pdfjs.GlobalWorkerOptions.workerSrc = "/vendor/pdf.worker.mjs";

function mulMatrix(m1, m2) {
  return [
    m1[0] * m2[0] + m1[2] * m2[1],
    m1[1] * m2[0] + m1[3] * m2[1],
    m1[0] * m2[2] + m1[2] * m2[3],
    m1[1] * m2[2] + m1[3] * m2[3],
    m1[0] * m2[4] + m1[2] * m2[5] + m1[4],
    m1[1] * m2[4] + m1[3] * m2[5] + m1[5]
  ];
}

function unitSquareRect(ctm, maxW, maxH) {
  const corners = [[0, 0], [1, 0], [0, 1], [1, 1]];
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  for (const [x, y] of corners) {
    const tx = ctm[0] * x + ctm[2] * y + ctm[4];
    const ty = ctm[1] * x + ctm[3] * y + ctm[5];
    if (tx < minX) minX = tx;
    if (ty < minY) minY = ty;
    if (tx > maxX) maxX = tx;
    if (ty > maxY) maxY = ty;
  }
  const x = Math.max(0, minX);
  const y = Math.max(0, minY);
  const right = Math.min(maxW, maxX);
  const bottom = Math.min(maxH, maxY);
  return { x, y, width: Math.max(0, right - x), height: Math.max(0, bottom - y) };
}

async function getInsertedImageRects(page, viewport, pixelRatio) {
  try {
    const opList = await page.getOperatorList();
    const OPS = pdfjs.OPS;
    const stack = [viewport.transform];
    const rects = [];
    const pageArea = Math.max(1, viewport.width * viewport.height);
    const { fnArray, argsArray } = opList;
    for (let i = 0; i < fnArray.length; i++) {
      const fn = fnArray[i];
      const args = argsArray[i];
      if (fn === OPS.save) {
        stack.push(stack[stack.length - 1]);
      } else if (fn === OPS.restore) {
        if (stack.length > 1) stack.pop();
      } else if (fn === OPS.transform && args) {
        stack[stack.length - 1] = mulMatrix(stack[stack.length - 1], args);
      } else if (fn === OPS.paintFormXObjectBegin) {
        const top = stack[stack.length - 1];
        stack.push(Array.isArray(args?.[0]) ? mulMatrix(top, args[0]) : top);
      } else if (fn === OPS.paintFormXObjectEnd) {
        if (stack.length > 1) stack.pop();
      } else if (
        fn === OPS.paintImageXObject ||
        fn === OPS.paintInlineImageXObject ||
        fn === OPS.paintImageXObjectRepeat
      ) {
        const rect = unitSquareRect(stack[stack.length - 1], viewport.width, viewport.height);
        if (rect.width * rect.height >= pageArea * 0.75) {
          // Full-page scanned / image-based PDF: smart-invert the entire page.
          return [];
        }
        if (rect.width >= 8 && rect.height >= 8) {
          rects.push({
            x: rect.x * pixelRatio,
            y: rect.y * pixelRatio,
            width: rect.width * pixelRatio,
            height: rect.height * pixelRatio
          });
        }
      }
    }
    return rects;
  } catch {
    return [];
  }
}

export async function openPdf(url, headers, reader, scroll, onRender, shouldSmartInvert = () => false) {
  const task = pdfjs.getDocument({
    url, httpHeaders: headers, rangeChunkSize: 65536, cMapUrl: "/vendor/cmaps/", cMapPacked: true,
    standardFontDataUrl: "/vendor/standard_fonts/", wasmUrl: "/vendor/wasm/", iccUrl: "/vendor/iccs/",
    isEvalSupported: false, enableXfa: false
  });
  let document;
  try {
    document = await task.promise;
  } catch (error) {
    await task.destroy().catch(() => {});
    throw error;
  }
  const first = await document.getPage(1);
  const sample = first.getViewport({ scale: 1 });
  let zoom = null;
  let revision = 0;
  let closed = false;
  let useTick = 0;
  const MAX_RENDERED_PAGES = 12;
  const entries = [];
  const observer = new IntersectionObserver(records => {
    for (const record of records) {
      const idx = Number(record.target.dataset.unit);
      const entry = entries[idx];
      if (!entry) continue;
      entry.visible = record.isIntersecting;
      if (record.isIntersecting) {
        entry.lastUsed = ++useTick;
        render(idx).catch(console.error);
      }
    }
  }, { root: scroll, rootMargin: "480px" });

  function evictPages() {
    const rendered = entries.filter(entry => entry.pending && !entry.visible);
    const totalRendered = entries.filter(entry => entry.pending).length;
    if (totalRendered <= MAX_RENDERED_PAGES) return;
    rendered.sort((a, b) => a.lastUsed - b.lastUsed);
    let excess = totalRendered - MAX_RENDERED_PAGES;
    for (const entry of rendered) {
      if (excess <= 0) break;
      entry.cancel?.();
      entry.pending = null;
      entry.cancel = null;
      if (entry.canvas) {
        entry.canvas.width = 0;
        entry.canvas.height = 0;
        entry.canvas = null;
      }
      entry.element.replaceChildren();
      excess--;
    }
  }

  function scale() {
    return zoom ?? Math.max(.2, (scroll.clientWidth - 36) / Math.max(1, sample.width));
  }

  function dimensions(element, viewport) {
    element.style.width = viewport.width + "px";
    element.style.height = viewport.height + "px";
    element.style.setProperty("--total-scale-factor", viewport.scale);
    element.style.setProperty("--scale-factor", viewport.scale);
  }

  function render(index) {
    const entry = entries[index];
    if (!entry || closed) return Promise.resolve();
    entry.lastUsed = ++useTick;
    if (entry.pending) return entry.pending;
    const version = revision;
    const target = entry.element;
    entry.pending = (async () => {
      const page = await document.getPage(index + 1);
      if (closed || version !== revision) return;
      entry.baseViewport = page.getViewport({ scale: 1 });
      const viewport = page.getViewport({ scale: scale() });
      dimensions(target, viewport);
      const canvas = window.document.createElement("canvas");
      const pixelRatio = Math.min(Math.max(window.devicePixelRatio || 1, 2), 2.5, Math.sqrt(12000000 / Math.max(1, viewport.width * viewport.height)));
      canvas.width = Math.ceil(viewport.width * pixelRatio);
      canvas.height = Math.ceil(viewport.height * pixelRatio);
      canvas.style.width = viewport.width + "px";
      canvas.style.height = viewport.height + "px";
      entry.canvas = canvas;
      const layer = window.document.createElement("div");
      layer.className = "textLayer";
      const text = new pdfjs.TextLayer({ textContentSource: page.streamTextContent(), container: layer, viewport });
      target.replaceChildren(canvas, layer);
      const raster = page.render({ canvas, viewport, transform: [pixelRatio, 0, 0, pixelRatio, 0, 0] });
      const opsPromise = shouldSmartInvert()
        ? getInsertedImageRects(page, viewport, pixelRatio)
        : Promise.resolve([]);
      entry.cancel = () => { raster.cancel(); text.cancel(); };
      const [, , preserveRects] = await Promise.all([raster.promise, text.render(), opsPromise]);
      if (closed || version !== revision) return;
      if (shouldSmartInvert()) applySmartInvertToCanvas(canvas, preserveRects);
      evictPages();
      onRender();
    })().catch(error => {
      if (!closed && version === revision) {
        entry.pending = null;
        if (entry.canvas) {
          entry.canvas.width = 0;
          entry.canvas.height = 0;
          entry.canvas = null;
        }
        throw error;
      }
    });
    return entry.pending;
  }

  function reset() {
    revision++;
    const factor = scale();
    for (const entry of entries) {
      entry.cancel?.();
      entry.pending = null;
      entry.cancel = null;
      if (entry.canvas) {
        entry.canvas.width = 0;
        entry.canvas.height = 0;
        entry.canvas = null;
      }
      entry.element.replaceChildren();
      dimensions(entry.element, {
        width: entry.baseViewport.width * factor,
        height: entry.baseViewport.height * factor,
        scale: factor
      });
      observer.unobserve(entry.element);
      observer.observe(entry.element);
    }
  }

  for (let i = 0; i < document.numPages; i++) {
    const element = window.document.createElement("section");
    element.className = "pdf-page";
    element.dataset.unit = i;
    element.setAttribute("aria-label", "第 " + (i + 1) + " 页");
    dimensions(element, first.getViewport({ scale: scale() }));
    reader.append(element);
    entries.push({ element, baseViewport: sample, pending: null, cancel: null, canvas: null, visible: false, lastUsed: 0 });
    observer.observe(element);
  }

  async function outline() {
    const items = [];
    async function visit(nodes, depth) {
      for (const node of nodes ?? []) {
        try {
          const destination = typeof node.dest === "string" ? await document.getDestination(node.dest) : node.dest;
          const reference = destination?.[0];
          if (reference != null) {
            const index = typeof reference === "number" ? reference : await document.getPageIndex(reference);
            items.push({ title: "　".repeat(Math.min(depth, 4)) + node.title, index });
          }
        } catch { /* Some document outlines contain unresolved destinations. */ }
        await visit(node.items, depth + 1);
      }
    }
    await visit(await document.getOutline(), 0);
    return items;
  }

  return {
    render, outline,
    count: document.numPages,
    fit() { zoom = null; reset(); },
    zoom(by) { zoom = Math.max(.25, Math.min(5, scale() * by)); reset(); },
    resize() { if (zoom === null) reset(); },
    refreshTheme() { reset(); },
    async text(index) {
      const page = await document.getPage(index + 1);
      const data = await page.getTextContent();
      return data.items.map(item => item.str ?? "").join("");
    },
    close() {
      closed = true;
      observer.disconnect();
      for (const entry of entries) {
        entry.cancel?.();
        if (entry.canvas) {
          entry.canvas.width = 0;
          entry.canvas.height = 0;
          entry.canvas = null;
        }
      }
      task.destroy().catch(console.error);
    }
  };
}
