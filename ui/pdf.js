import * as pdfjs from "/vendor/pdf.mjs";

pdfjs.GlobalWorkerOptions.workerSrc = "/vendor/pdf.worker.mjs";

export async function openPdf(url, headers, reader, scroll, onRender) {
  const task = pdfjs.getDocument({
    url, httpHeaders: headers, cMapUrl: "/vendor/cmaps/", cMapPacked: true,
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
  const entries = [];
  const observer = new IntersectionObserver(records => {
    for (const record of records) {
      if (record.isIntersecting) render(Number(record.target.dataset.unit)).catch(console.error);
    }
  }, { root: scroll, rootMargin: "480px" });

  function scale() {
    return zoom ?? Math.max(.2, (scroll.clientWidth - 36) / sample.width);
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
    if (entry.pending) return entry.pending;
    const version = revision;
    const target = entry.element;
    entry.pending = (async () => {
      const page = await document.getPage(index + 1);
      if (closed || version !== revision) return;
      const viewport = page.getViewport({ scale: scale() });
      dimensions(target, viewport);
      const canvas = window.document.createElement("canvas");
      const pixelRatio = Math.min(window.devicePixelRatio || 1, 2, Math.sqrt(12000000 / (viewport.width * viewport.height)));
      canvas.width = Math.ceil(viewport.width * pixelRatio);
      canvas.height = Math.ceil(viewport.height * pixelRatio);
      canvas.style.width = viewport.width + "px";
      canvas.style.height = viewport.height + "px";
      const layer = window.document.createElement("div");
      layer.className = "textLayer";
      const text = new pdfjs.TextLayer({ textContentSource: page.streamTextContent(), container: layer, viewport });
      target.replaceChildren(canvas, layer);
      const raster = page.render({ canvas, viewport, transform: [pixelRatio, 0, 0, pixelRatio, 0, 0] });
      entry.cancel = () => { raster.cancel(); text.cancel(); };
      await Promise.all([raster.promise, text.render()]);
      if (!closed && version === revision) onRender();
    })().catch(error => {
      if (!closed && version === revision) {
        entry.pending = null;
        throw error;
      }
    });
    return entry.pending;
  }

  function reset() {
    revision++;
    for (const entry of entries) {
      entry.cancel?.();
      entry.pending = null;
      entry.cancel = null;
      entry.element.replaceChildren();
      dimensions(entry.element, first.getViewport({ scale: scale() }));
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
    entries.push({ element, pending: null, cancel: null });
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
    async text(index) {
      const page = await document.getPage(index + 1);
      const data = await page.getTextContent();
      return data.items.map(item => item.str ?? "").join("");
    },
    close() {
      closed = true;
      observer.disconnect();
      for (const entry of entries) entry.cancel?.();
      task.destroy().catch(console.error);
    }
  };
}
