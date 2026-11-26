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

function rectArea(rect) {
  return Math.max(0, rect.width) * Math.max(0, rect.height);
}

async function collectPageImages(page, viewport, pixelRatio) {
  try {
    const opList = await page.getOperatorList();
    const OPS = pdfjs.OPS;
    const stack = [viewport.transform];
    const preserve = [];
    const hitImages = [];
    let hasFullPage = false;
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
        if (rect.width >= 8 && rect.height >= 8) {
          hitImages.push({
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            objId: fn === OPS.paintInlineImageXObject ? null : (typeof args?.[0] === "string" ? args[0] : null),
            inlineImg: fn === OPS.paintInlineImageXObject ? (args?.[0] ?? null) : null
          });
          if (rect.width * rect.height >= pageArea * 0.75) {
            // Full-page scanned / image-based PDF: smart-invert the entire page.
            hasFullPage = true;
          } else {
            preserve.push({
              x: rect.x * pixelRatio,
              y: rect.y * pixelRatio,
              width: rect.width * pixelRatio,
              height: rect.height * pixelRatio
            });
          }
        }
      }
    }
    return { preserveRects: hasFullPage ? [] : preserve, hitImages };
  } catch {
    return { preserveRects: [], hitImages: [] };
  }
}

function canvasToPngBlob(canvas) {
  return new Promise((resolve, reject) => {
    canvas.toBlob(blob => {
      if (blob) resolve(blob);
      else reject(new Error("无法导出图像"));
    }, "image/png");
  });
}

function decodePdfImageObject(imgObj) {
  if (!imgObj) return null;
  if (imgObj.bitmap && imgObj.bitmap.width > 0 && imgObj.bitmap.height > 0) {
    const out = window.document.createElement("canvas");
    out.width = imgObj.bitmap.width;
    out.height = imgObj.bitmap.height;
    out.getContext("2d").drawImage(imgObj.bitmap, 0, 0);
    return out;
  }
  const { data, width, height } = imgObj;
  if (!data || !width || !height) return null;
  const out = window.document.createElement("canvas");
  out.width = width;
  out.height = height;
  const ctx = out.getContext("2d");
  const imageData = ctx.createImageData(width, height);
  const dst = imageData.data;
  const pixels = width * height;
  if (data.length === pixels * 4) {
    dst.set(data);
  } else if (data.length === pixels * 3) {
    for (let s = 0, d = 0; d < dst.length; s += 3, d += 4) {
      dst[d] = data[s];
      dst[d + 1] = data[s + 1];
      dst[d + 2] = data[s + 2];
      dst[d + 3] = 255;
    }
  } else if (data.length === pixels) {
    for (let s = 0, d = 0; s < pixels; s++, d += 4) {
      const v = data[s];
      dst[d] = v;
      dst[d + 1] = v;
      dst[d + 2] = v;
      dst[d + 3] = 255;
    }
  } else if (data.length === Math.ceil(width / 8) * height) {
    const rowBytes = Math.ceil(width / 8);
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const bit = (data[y * rowBytes + (x >> 3)] >> (7 - (x & 7))) & 1;
        const v = bit ? 255 : 0;
        const d = (y * width + x) * 4;
        dst[d] = v;
        dst[d + 1] = v;
        dst[d + 2] = v;
        dst[d + 3] = 255;
      }
    }
  } else {
    return null;
  }
  ctx.putImageData(imageData, 0, 0);
  return out;
}

function getPdfImageObject(page, img) {
  if (!page || !img) return null;
  if (img.inlineImg) return img.inlineImg;
  if (!img.objId) return null;
  const store = img.objId.startsWith("g_") ? page.commonObjs : page.objs;
  try {
    return store.has(img.objId) ? store.get(img.objId) : null;
  } catch {
    return null;
  }
}

async function extractPdfImageBlob(page, img, entryCanvas, viewport) {
  const direct = decodePdfImageObject(getPdfImageObject(page, img));
  if (direct) return canvasToPngBlob(direct);
  if (!entryCanvas || !viewport) throw new Error("图像尚未加载");
  const sxScale = entryCanvas.width / Math.max(1, viewport.width);
  const syScale = entryCanvas.height / Math.max(1, viewport.height);
  const sx = Math.max(0, Math.floor(img.x * sxScale));
  const sy = Math.max(0, Math.floor(img.y * syScale));
  const sw = Math.max(1, Math.min(entryCanvas.width - sx, Math.ceil(img.width * sxScale)));
  const sh = Math.max(1, Math.min(entryCanvas.height - sy, Math.ceil(img.height * syScale)));
  const out = window.document.createElement("canvas");
  out.width = sw;
  out.height = sh;
  out.getContext("2d").drawImage(entryCanvas, sx, sy, sw, sh, 0, 0, sw, sh);
  return canvasToPngBlob(out);
}

const BARE_URL_RE = /https?:\/\/[^\s<>"'`()[\]{}，。、；：！？（）【】《》]+/;

function stripUrlTrailingPunct(url) {
  return url.replace(/[.,;:!?]+$/, "");
}

export async function openPdf(
  url,
  headers,
  reader,
  scroll,
  onRender,
  shouldSmartInvert = () => false,
  onExternalLink = () => {},
  onInternalPage = () => {}
) {
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
      entry.hasText = false;
      entry.images = [];
      entry.viewport = null;
      entry.page = null;
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
      const linksLayer = window.document.createElement("div");
      linksLayer.className = "pdf-links";
      const text = new pdfjs.TextLayer({ textContentSource: page.streamTextContent(), container: layer, viewport });
      target.replaceChildren(canvas, layer, linksLayer);
      const raster = page.render({ canvas, viewport, transform: [pixelRatio, 0, 0, pixelRatio, 0, 0] });
      const opsPromise = collectPageImages(page, viewport, pixelRatio);
      const annosPromise = page.getAnnotations().catch(() => []);
      entry.cancel = () => { raster.cancel(); text.cancel(); };
      const [, , imageInfo, annotations] = await Promise.all([raster.promise, text.render(), opsPromise, annosPromise]);
      if (closed || version !== revision) return;
      entry.hasText = true;
      entry.images = imageInfo.hitImages;
      entry.viewport = viewport;
      entry.page = page;
      for (const anno of annotations) {
        if (anno?.subtype !== "Link" || !Array.isArray(anno.rect)) continue;
        const r = viewport.convertToViewportRectangle(anno.rect);
        const left = Math.min(r[0], r[2]);
        const top = Math.min(r[1], r[3]);
        const width = Math.abs(r[2] - r[0]);
        const height = Math.abs(r[3] - r[1]);
        if (width < 2 || height < 2) continue;
        const targetUrl = anno.url || anno.unsafeUrl || "";
        if (targetUrl) {
          const anchor = window.document.createElement("a");
          anchor.className = "pdf-link";
          anchor.href = "#";
          anchor.title = targetUrl;
          anchor.style.cssText = `left:${left}px;top:${top}px;width:${width}px;height:${height}px;`;
          anchor.addEventListener("click", event => {
            event.preventDefault();
            onExternalLink(targetUrl);
          });
          linksLayer.append(anchor);
        } else if (anno.dest) {
          const anchor = window.document.createElement("a");
          anchor.className = "pdf-link";
          anchor.href = "#";
          anchor.style.cssText = `left:${left}px;top:${top}px;width:${width}px;height:${height}px;`;
          anchor.addEventListener("click", async event => {
            event.preventDefault();
            try {
              const dest = typeof anno.dest === "string" ? await document.getDestination(anno.dest) : anno.dest;
              const ref = dest?.[0];
              if (ref != null) {
                const targetPage = typeof ref === "number" ? ref : await document.getPageIndex(ref);
                onInternalPage(targetPage);
              }
            } catch { /* Ignore malformed internal destination. */ }
          });
          linksLayer.append(anchor);
        }
      }
      for (const span of layer.querySelectorAll("span")) {
        const match = BARE_URL_RE.exec(span.textContent || "");
        if (!match) continue;
        const matchedUrl = stripUrlTrailingPunct(match[0]);
        if (!matchedUrl) continue;
        span.style.cursor = "pointer";
        span.title = matchedUrl;
        span.addEventListener("click", event => {
          if (window.getSelection()?.toString()) return;
          event.preventDefault();
          onExternalLink(matchedUrl);
        });
      }
      if (shouldSmartInvert()) applySmartInvertToCanvas(canvas, imageInfo.preserveRects);
      evictPages();
      onRender();
    })().catch(error => {
      if (!closed && version === revision) {
        entry.pending = null;
        entry.hasText = false;
        entry.images = [];
        entry.viewport = null;
        entry.page = null;
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
      entry.hasText = false;
      entry.images = [];
      entry.viewport = null;
      entry.page = null;
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
    entries.push({ element, baseViewport: sample, pending: null, cancel: null, canvas: null, images: [], viewport: null, page: null, hasText: false, visible: false, lastUsed: 0 });
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
    imageAt(pageIndex, clientX, clientY) {
      const entry = entries[pageIndex];
      if (!entry?.canvas) return null;
      const box = entry.element.getBoundingClientRect();
      const px = clientX - box.left;
      const py = clientY - box.top;
      let matched = null;
      let matchedArea = Infinity;
      for (const img of entry.images ?? []) {
        if (px >= img.x && px <= img.x + img.width && py >= img.y && py <= img.y + img.height) {
          const area = rectArea(img);
          if (area <= matchedArea) {
            matched = img;
            matchedArea = area;
          }
        }
      }
      if (matched) {
        return {
          kind: "image",
          filename: "page-" + (pageIndex + 1) + "-image.png",
          extractPng: () => extractPdfImageBlob(entry.page, matched, entry.canvas, entry.viewport)
        };
      }
      return {
        kind: "page",
        filename: "page-" + (pageIndex + 1) + ".png",
        extractPng: () => canvasToPngBlob(entry.canvas)
      };
    },
    missingTextCount() {
      return entries.filter(entry => !entry.hasText).length;
    },
    async ensureAllTextLayers(onProgress, signal) {
      const missing = [];
      for (let i = 0; i < entries.length; i++) {
        if (!entries[i].hasText) missing.push(i);
      }
      const total = missing.length;
      if (!total) return;
      let cursor = 0;
      let done = 0;
      const version = revision;
      await Promise.all(Array.from({ length: Math.min(4, total) }, async () => {
        while (cursor < missing.length && !signal?.aborted && !closed && version === revision) {
          const index = missing[cursor++];
          const entry = entries[index];
          if (!entry || entry.hasText) {
            onProgress?.(++done, total);
            continue;
          }
          const page = await document.getPage(index + 1);
          if (signal?.aborted || closed || version !== revision) return;
          const viewport = page.getViewport({ scale: scale() });
          entry.baseViewport = page.getViewport({ scale: 1 });
          dimensions(entry.element, viewport);
          const layer = window.document.createElement("div");
          layer.className = "textLayer";
          const text = new pdfjs.TextLayer({ textContentSource: page.streamTextContent(), container: layer, viewport });
          await text.render();
          if (signal?.aborted || closed || version !== revision) return;
          if (!entry.pending && !entry.hasText) {
            entry.element.replaceChildren(layer);
            entry.hasText = true;
          }
          onProgress?.(++done, total);
        }
      }));
    },
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
