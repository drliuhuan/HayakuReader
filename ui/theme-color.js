const BG_RGB = [30, 33, 31]; // #1e211f
const FG_RGB = [230, 233, 228]; // #e6e9e4

function srgbToLinear(c) {
  const u = c / 255;
  return u <= 0.04045 ? u / 12.92 : ((u + 0.055) / 1.055) ** 2.4;
}

function linearToSrgb(x) {
  const clamped = Math.max(0, Math.min(1, x));
  const s = clamped <= 0.0031308 ? 12.92 * clamped : 1.055 * (clamped ** (1 / 2.4)) - 0.055;
  return Math.round(s * 255);
}

function relLum(r, g, b) {
  return 0.2126 * srgbToLinear(r) + 0.7152 * srgbToLinear(g) + 0.0722 * srgbToLinear(b);
}

const BG_LUM = relLum(BG_RGB[0], BG_RGB[1], BG_RGB[2]);
const [BG_L] = rgbToOklab(BG_RGB[0], BG_RGB[1], BG_RGB[2]);
const [FG_L] = rgbToOklab(FG_RGB[0], FG_RGB[1], FG_RGB[2]);

function contrastOnDark(lum) {
  return (Math.max(lum, BG_LUM) + 0.05) / (Math.min(lum, BG_LUM) + 0.05);
}

function rgbToOklab(r, g, b) {
  const lr = srgbToLinear(r);
  const lg = srgbToLinear(g);
  const lb = srgbToLinear(b);
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  return [
    0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s
  ];
}

function oklabToLinearRgb(L, a, b) {
  const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
  const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
  const s_ = L - 0.0894841775 * a - 1.2914855480 * b;
  const l = l_ * l_ * l_;
  const m = m_ * m_ * m_;
  const s = s_ * s_ * s_;
  return [
    +4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s
  ];
}

function inGamut(rgb) {
  return rgb[0] >= -1e-4 && rgb[0] <= 1.0001
    && rgb[1] >= -1e-4 && rgb[1] <= 1.0001
    && rgb[2] >= -1e-4 && rgb[2] <= 1.0001;
}

function oklchToSrgb(L, C, cosH, sinH) {
  const direct = oklabToLinearRgb(L, C * cosH, C * sinH);
  if (inGamut(direct)) {
    return [linearToSrgb(direct[0]), linearToSrgb(direct[1]), linearToSrgb(direct[2])];
  }
  let low = 0;
  let high = C;
  for (let i = 0; i < 12; i++) {
    const mid = (low + high) * 0.5;
    if (inGamut(oklabToLinearRgb(L, mid * cosH, mid * sinH))) low = mid;
    else high = mid;
  }
  const clipped = oklabToLinearRgb(L, low * cosH, low * sinH);
  return [linearToSrgb(clipped[0]), linearToSrgb(clipped[1]), linearToSrgb(clipped[2])];
}

function smoothstep(edge0, edge1, x) {
  const t = Math.max(0, Math.min(1, (x - edge0) / (edge1 - edge0)));
  return t * t * (3 - 2 * t);
}

function liftColor(L, C, cosH, sinH, targetContrast) {
  let low = L;
  let high = 0.88;
  for (let i = 0; i < 14; i++) {
    const mid = (low + high) * 0.5;
    const [r, g, b] = oklchToSrgb(mid, C, cosH, sinH);
    if (contrastOnDark(relLum(r, g, b)) >= targetContrast) high = mid;
    else low = mid;
  }
  return oklchToSrgb(high, C, cosH, sinH);
}

export function mapRgbDark(r, g, b, isBackground = false) {
  const [L, a, b_] = rgbToOklab(r, g, b);
  const C = Math.hypot(a, b_);
  const L_neutral = FG_L + (BG_L - FG_L) * (L ** 0.9);
  const neutral = oklchToSrgb(L_neutral, 0, 1, 0);
  if (C < 1e-4) return neutral;

  const cosH = a / C;
  const sinH = b_ / C;
  let chromatic;

  if (L > 0.84 && C < 0.14) {
    // Light tinted backgrounds / highlights become soft dark tints preserving hue.
    const targetL = 0.25 + (1 - L) * 0.85;
    chromatic = oklchToSrgb(targetL, Math.min(C, 0.11), cosH, sinH);
  } else if (isBackground) {
    chromatic = L > 0.52
      ? oklchToSrgb(Math.max(0.24, 0.52 - (L - 0.52) * 0.5), Math.min(C, 0.14), cosH, sinH)
      : [r, g, b];
  } else {
    const contrast = contrastOnDark(relLum(r, g, b));
    if (contrast >= 4.5 || (L >= 0.55 && C >= 0.14 && contrast >= 3.2)) {
      chromatic = [r, g, b];
    } else {
      chromatic = liftColor(L, Math.max(C, 0.08), cosH, sinH, 4.5);
    }
  }

  const wNeutral = 1 - smoothstep(0.025, 0.065, C);
  if (wNeutral <= 0) return chromatic;
  if (wNeutral >= 1) return neutral;
  return [
    Math.round(chromatic[0] * (1 - wNeutral) + neutral[0] * wNeutral),
    Math.round(chromatic[1] * (1 - wNeutral) + neutral[1] * wNeutral),
    Math.round(chromatic[2] * (1 - wNeutral) + neutral[2] * wNeutral)
  ];
}

const LUT_SIZE = 33;
let lutData = null;
const WHITE_MAPPED = mapRgbDark(255, 255, 255, false);
const BLACK_MAPPED = mapRgbDark(0, 0, 0, false);

function ensureLut() {
  if (lutData) return lutData;
  lutData = new Uint8Array(LUT_SIZE * LUT_SIZE * LUT_SIZE * 3);
  const max = LUT_SIZE - 1;
  let offset = 0;
  for (let bi = 0; bi < LUT_SIZE; bi++) {
    const b = Math.round((bi / max) * 255);
    for (let gi = 0; gi < LUT_SIZE; gi++) {
      const g = Math.round((gi / max) * 255);
      for (let ri = 0; ri < LUT_SIZE; ri++) {
        const r = Math.round((ri / max) * 255);
        const [mr, mg, mb] = mapRgbDark(r, g, b, false);
        lutData[offset++] = mr;
        lutData[offset++] = mg;
        lutData[offset++] = mb;
      }
    }
  }
  return lutData;
}

export function applySmartInvertToCanvas(canvas, preserveRects = []) {
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx || !canvas.width || !canvas.height) return;

  const snapshots = [];
  for (const rect of preserveRects) {
    const x = Math.max(0, Math.floor(rect.x));
    const y = Math.max(0, Math.floor(rect.y));
    const right = Math.min(canvas.width, Math.ceil(rect.x + rect.width));
    const bottom = Math.min(canvas.height, Math.ceil(rect.y + rect.height));
    const w = right - x;
    const h = bottom - y;
    if (w > 0 && h > 0) {
      snapshots.push({ x, y, data: ctx.getImageData(x, y, w, h) });
    }
  }

  const frame = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const pixels = frame.data;
  const lut = ensureLut();
  const max = LUT_SIZE - 1;
  const scale = max / 255;
  const strideG = LUT_SIZE * 3;
  const strideB = LUT_SIZE * strideG;
  const wr = WHITE_MAPPED[0], wg = WHITE_MAPPED[1], wb = WHITE_MAPPED[2];
  const br = BLACK_MAPPED[0], bg = BLACK_MAPPED[1], bb = BLACK_MAPPED[2];

  for (let i = 0; i < pixels.length; i += 4) {
    if (pixels[i + 3] === 0) continue;
    const r = pixels[i];
    const g = pixels[i + 1];
    const b = pixels[i + 2];
    if (r === 255 && g === 255 && b === 255) {
      pixels[i] = wr;
      pixels[i + 1] = wg;
      pixels[i + 2] = wb;
      continue;
    }
    if (r === 0 && g === 0 && b === 0) {
      pixels[i] = br;
      pixels[i + 1] = bg;
      pixels[i + 2] = bb;
      continue;
    }
    const rf = r * scale;
    const gf = g * scale;
    const bf = b * scale;
    const r0 = rf < max ? (rf | 0) : max - 1;
    const g0 = gf < max ? (gf | 0) : max - 1;
    const b0 = bf < max ? (bf | 0) : max - 1;
    const dr = rf - r0;
    const dg = gf - g0;
    const db = bf - b0;
    const base = b0 * strideB + g0 * strideG + r0 * 3;
    const c100 = base + 3;
    const c010 = base + strideG;
    const c110 = c010 + 3;
    const c001 = base + strideB;
    const c101 = c001 + 3;
    const c011 = c001 + strideG;
    const c111 = c011 + 3;
    const w00 = (1 - dr) * (1 - dg);
    const w10 = dr * (1 - dg);
    const w01 = (1 - dr) * dg;
    const w11 = dr * dg;
    const w0 = 1 - db;
    const w1 = db;

    pixels[i] = ((lut[base] * w00 + lut[c100] * w10 + lut[c010] * w01 + lut[c110] * w11) * w0
      + (lut[c001] * w00 + lut[c101] * w10 + lut[c011] * w01 + lut[c111] * w11) * w1 + 0.5) | 0;
    pixels[i + 1] = ((lut[base + 1] * w00 + lut[c100 + 1] * w10 + lut[c010 + 1] * w01 + lut[c110 + 1] * w11) * w0
      + (lut[c001 + 1] * w00 + lut[c101 + 1] * w10 + lut[c011 + 1] * w01 + lut[c111 + 1] * w11) * w1 + 0.5) | 0;
    pixels[i + 2] = ((lut[base + 2] * w00 + lut[c100 + 2] * w10 + lut[c010 + 2] * w01 + lut[c110 + 2] * w11) * w0
      + (lut[c001 + 2] * w00 + lut[c101 + 2] * w10 + lut[c011 + 2] * w01 + lut[c111 + 2] * w11) * w1 + 0.5) | 0;
  }
  ctx.putImageData(frame, 0, 0);

  for (const snap of snapshots) {
    ctx.putImageData(snap.data, snap.x, snap.y);
  }
}

let probeCtx = null;

function parseCssColor(value) {
  if (!value || /currentcolor|inherit|initial|unset|transparent/i.test(value.trim())) return null;
  if (!probeCtx) {
    const probe = document.createElement("canvas");
    probe.width = probe.height = 1;
    probeCtx = probe.getContext("2d", { willReadFrequently: true });
  }
  probeCtx.clearRect(0, 0, 1, 1);
  probeCtx.fillStyle = "rgba(0,0,0,0)";
  probeCtx.fillStyle = value;
  const normalized = probeCtx.fillStyle;
  if (normalized === "rgba(0, 0, 0, 0)") return null;
  probeCtx.fillRect(0, 0, 1, 1);
  const data = probeCtx.getImageData(0, 0, 1, 1).data;
  if (data[3] === 0) return null;
  return [data[0], data[1], data[2], data[3] / 255];
}

export function adaptCssColor(value, isBackground = false) {
  const rgba = parseCssColor(value);
  if (!rgba) return value;
  const [r, g, b, a] = rgba;
  const [nr, ng, nb] = mapRgbDark(r, g, b, isBackground);
  return a < 0.999
    ? `rgba(${nr}, ${ng}, ${nb}, ${Number(a.toFixed(3))})`
    : `rgb(${nr}, ${ng}, ${nb})`;
}
