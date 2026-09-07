// 生成 1024x1024 占位应用图标（无第三方依赖）。
// 用法: node scripts/gen-icon.mjs [输出路径]
// 产物: 默认 src-tauri/icons/app-icon.png，可被 `npx tauri icon` 用作源图。
import { deflateSync } from 'node:zlib'
import { writeFileSync, mkdirSync } from 'node:fs'
import { dirname, resolve } from 'node:path'

const SIZE = 1024
const outPath = resolve(process.argv[2] ?? 'src-tauri/icons/app-icon.png')

// ---- minimal PNG writer ----
const crcTable = (() => {
  const t = new Uint32Array(256)
  for (let n = 0; n < 256; n++) {
    let c = n
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    t[n] = c >>> 0
  }
  return t
})()

function crc32(buf) {
  let c = 0xffffffff
  for (let i = 0; i < buf.length; i++) c = crcTable[(c ^ buf[i]) & 0xff] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

function chunk(type, data) {
  const len = Buffer.alloc(4)
  len.writeUInt32BE(data.length)
  const typeBuf = Buffer.from(type, 'ascii')
  const crcBuf = Buffer.alloc(4)
  crcBuf.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])))
  return Buffer.concat([len, typeBuf, data, crcBuf])
}

function writePng(path, rgba, size) {
  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(size, 0)
  ihdr.writeUInt32BE(size, 4)
  ihdr[8] = 8 // bit depth
  ihdr[9] = 6 // color type RGBA
  // raw scanlines with filter byte 0
  const stride = size * 4
  const raw = Buffer.alloc((stride + 1) * size)
  for (let y = 0; y < size; y++) {
    raw[y * (stride + 1)] = 0
    rgba.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride)
  }
  const png = Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ])
  mkdirSync(dirname(path), { recursive: true })
  writeFileSync(path, png)
  console.log(`icon written: ${path} (${png.length} bytes)`)
}

// ---- pixel art: 深色圆角底 + 柱状图风格的 3 根"token"柱 ----
const rgba = Buffer.alloc(SIZE * SIZE * 4)
const bg = [30, 33, 40] // #1e2128
const barColors = [
  [37, 99, 235], // #2563eb
  [96, 165, 250], // #60a5fa
  [16, 185, 129], // #10b981
]
const cx = SIZE / 2
const cy = SIZE / 2
const radius = SIZE * 0.44
const R2 = radius * radius

for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    const dx = x - cx + 0.5
    const dy = y - cy + 0.5
    const inside = dx * dx + dy * dy <= R2
    const i = (y * SIZE + x) * 4
    if (!inside) {
      rgba[i] = rgba[i + 1] = rgba[i + 2] = rgba[i + 3] = 0
      continue
    }
    rgba[i] = bg[0]
    rgba[i + 1] = bg[1]
    rgba[i + 2] = bg[2]
    rgba[i + 3] = 255
  }
}

// bars: 宽 barW, 高按比例; 底部位于 cy + base
const barW = 92
const base = cy + 200
const bars = [
  { color: barColors[0], h: 240 },
  { color: barColors[1], h: 420 },
  { color: barColors[2], h: 330 },
]
const gap = 56
const totalW = bars.length * barW + (bars.length - 1) * gap
let bx = cx - totalW / 2

for (const bar of bars) {
  const x0 = Math.round(bx)
  const x1 = Math.round(bx + barW)
  const y0 = Math.round(base - bar.h)
  const y1 = Math.round(base)
  for (let y = y0; y < y1; y++) {
    for (let x = x0; x < x1; x++) {
      if (x < 0 || x >= SIZE || y < 0 || y >= SIZE) continue
      const dx = x - cx + 0.5
      const dy = y - cy + 0.5
      if (dx * dx + dy * dy > R2) continue
      const i = (y * SIZE + x) * 4
      rgba[i] = bar.color[0]
      rgba[i + 1] = bar.color[1]
      rgba[i + 2] = bar.color[2]
      rgba[i + 3] = 255
    }
  }
  bx += barW + gap
}

writePng(outPath, rgba, SIZE)
