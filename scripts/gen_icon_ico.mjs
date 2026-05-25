#!/usr/bin/env node
/**
 * Generate multi-size icon.ico (PNG-compressed entries) for winres/rc.exe.
 */
import { writeFileSync } from "fs";
import { join, dirname } from "path";
import { fileURLToPath } from "url";
import zlib from "zlib";

const __dirname = dirname(fileURLToPath(import.meta.url));
const assets = join(__dirname, "..", "assets");

const GOLD = { r: 201, g: 169, b: 78, a: 255 };

const POLYGONS = [
  [[22, 46], [54, 2], [86, 46], [80, 46], [54, 14], [28, 46]],
  [[34, 46], [54, 30], [74, 46], [74, 62], [34, 62]],
  [[30, 62], [78, 62], [82, 68], [26, 68]],
  [[26, 68], [82, 68], [86, 74], [22, 74]],
  [[22, 74], [86, 74], [90, 82], [18, 82]],
];

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function pointInTri(px, py, ax, ay, bx, by, cx, cy) {
  const d1 = (px - bx) * (ay - by) - (ax - bx) * (py - by);
  const d2 = (px - cx) * (by - cy) - (bx - cx) * (py - cy);
  const d3 = (px - ax) * (cy - ay) - (cx - ax) * (py - ay);
  const hasNeg = d1 < 0 || d2 < 0 || d3 < 0;
  const hasPos = d1 > 0 || d2 > 0 || d3 > 0;
  return !(hasNeg && hasPos);
}

function pointInPoly(px, py, poly, scale) {
  const pts = poly.map(([x, y]) => [x * scale, y * scale]);
  for (let i = 0; i < pts.length - 2; i++) {
    if (pointInTri(px, py, pts[0][0], pts[0][1], pts[i + 1][0], pts[i + 1][1], pts[i + 2][0], pts[i + 2][1]))
      return true;
  }
  return false;
}

function rasterize(size) {
  const scale = size / 108;
  const pixels = Buffer.alloc(size * size * 4, 0);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let filled = false;
      for (const poly of POLYGONS) {
        if (pointInPoly(x + 0.5, y + 0.5, poly, scale)) {
          filled = true;
          break;
        }
      }
      if (filled) {
        const i = (y * size + x) * 4;
        pixels[i] = GOLD.r;
        pixels[i + 1] = GOLD.g;
        pixels[i + 2] = GOLD.b;
        pixels[i + 3] = GOLD.a;
      }
    }
  }
  return pixels;
}

function pngChunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const t = Buffer.from(type);
  const body = Buffer.concat([t, data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, t, data, crc]);
}

function createPng(size, pixels) {
  const rows = [];
  for (let y = 0; y < size; y++) {
    const row = Buffer.alloc(1 + size * 4);
    row[0] = 0;
    for (let x = 0; x < size; x++) {
      const si = (y * size + x) * 4;
      const o = 1 + x * 4;
      row[o] = pixels[si];
      row[o + 1] = pixels[si + 1];
      row[o + 2] = pixels[si + 2];
      row[o + 3] = pixels[si + 3];
    }
    rows.push(row);
  }
  const raw = Buffer.concat(rows);
  const compressed = zlib.deflateSync(raw, { level: 9 });

  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;

  return Buffer.concat([
    signature,
    pngChunk("IHDR", ihdr),
    pngChunk("IDAT", compressed),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

function createIco(sizes) {
  const images = sizes.map((s) => ({ size: s, data: createPng(s, rasterize(s)) }));
  const count = images.length;
  const headerSize = 6 + count * 16;
  let dataOffset = headerSize;
  const parts = [Buffer.alloc(headerSize)];
  const header = parts[0];
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(count, 4);
  let entryOff = 6;
  for (const img of images) {
    const e = header;
    const dim = img.size >= 256 ? 0 : img.size;
    e[entryOff] = dim;
    e[entryOff + 1] = dim;
    e[entryOff + 2] = 0;
    e[entryOff + 3] = 0;
    e.writeUInt16LE(1, entryOff + 4);
    e.writeUInt16LE(32, entryOff + 6);
    e.writeUInt32LE(img.data.length, entryOff + 8);
    e.writeUInt32LE(dataOffset, entryOff + 12);
    dataOffset += img.data.length;
    entryOff += 16;
    parts.push(img.data);
  }
  return Buffer.concat(parts);
}

const sizes = [16, 32, 48, 256];
const ico = createIco(sizes);
const outPath = join(assets, "icon.ico");
writeFileSync(outPath, ico);
console.log(`Wrote ${outPath} (${ico.length} bytes, sizes: ${sizes.join(",")})`);
