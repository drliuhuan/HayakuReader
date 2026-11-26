export function openWord(metadata, contentRoute, request, reader, scroll, onRender) {
  const firstWidth = metadata.pages[0][0];
  const entries = [];
  const measure = document.createElement("canvas").getContext("2d");
  let zoom = null;
  let closed = false;
  const observer = new IntersectionObserver(records => {
    for (const record of records) {
      if (record.isIntersecting) render(Number(record.target.dataset.unit)).catch(console.error);
    }
  }, { root: scroll, rootMargin: "480px" });

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

  function render(index) {
    const entry = entries[index];
    if (!entry || closed) return Promise.resolve();
    if (!entry.pending) entry.pending = (async () => {
      const [runs, response] = await Promise.all([getText(index), request(contentRoute(index) + "&raster=true")]);
      const blob = await response.blob();
      if (closed) return;
      const picture = document.createElement("img");
      picture.alt = "";
      picture.draggable = false;
      entry.url = URL.createObjectURL(blob);
      picture.src = entry.url;
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
      entry.layer = layer;
      entry.element.replaceChildren(picture, layer);
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
    entries.push({ element, pending: null });
    observer.observe(element);
  }
  resize();

  return {
    render, resize,
    count: metadata.pages.length,
    fit() { zoom = null; resize(); },
    zoom(by) { zoom = Math.max(.25, Math.min(5, scale() * by)); resize(); },
    async text(index) {
      const runs = await getText(index);
      return runs.map(run => (run.line_break ? "\n" : "") + run.text).join("");
    },
    close() {
      closed = true;
      observer.disconnect();
      for (const entry of entries) {
        if (entry.url) URL.revokeObjectURL(entry.url);
      }
    }
  };
}
