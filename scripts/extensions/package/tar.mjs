// A small, deterministic tar writer (ustar). The same files always give the same
// bytes: entries are sorted by the caller, times and owners are zero, and modes
// are fixed, so a package can be rebuilt and compared by its SHA-256.

const BLOCK = 512;

function octal(value, width) {
  const text = value.toString(8);
  if (text.length > width - 1) throw new Error(`value ${value} does not fit a tar field`);
  return text.padStart(width - 1, '0') + '\0';
}

function put(header, offset, text, width) {
  const bytes = Buffer.from(text, 'utf8');
  if (bytes.length > width) throw new Error(`"${text}" does not fit a tar field`);
  bytes.copy(header, offset);
}

/** Split a path into the ustar `prefix` and `name` fields (name up to 100 bytes, prefix up to 155). */
function splitPath(path) {
  if (Buffer.byteLength(path) <= 100) return ['', path];
  for (let at = path.indexOf('/'); at !== -1; at = path.indexOf('/', at + 1)) {
    const prefix = path.slice(0, at);
    const name = path.slice(at + 1);
    if (Buffer.byteLength(prefix) <= 155 && Buffer.byteLength(name) <= 100) return [prefix, name];
  }
  throw new Error(`the path is too long for a package: ${path}`);
}

/** @param {{ path: string, data: Buffer }[]} entries regular files only */
export function createTar(entries) {
  const chunks = [];
  for (const { path, data } of entries) {
    const [prefix, name] = splitPath(path);
    const header = Buffer.alloc(BLOCK);
    put(header, 0, name, 100);
    put(header, 100, octal(0o644, 8), 8);
    put(header, 108, octal(0, 8), 8);
    put(header, 116, octal(0, 8), 8);
    put(header, 124, octal(data.length, 12), 12);
    put(header, 136, octal(0, 12), 12);
    header.fill(0x20, 148, 156); // the checksum counts itself as spaces
    header[156] = 0x30; // '0': a regular file
    put(header, 257, 'ustar\0', 6);
    put(header, 263, '00', 2);
    put(header, 345, prefix, 155);
    let sum = 0;
    for (const byte of header) sum += byte;
    put(header, 148, sum.toString(8).padStart(6, '0') + '\0 ', 8);
    chunks.push(header, data);
    const padding = (BLOCK - (data.length % BLOCK)) % BLOCK;
    if (padding) chunks.push(Buffer.alloc(padding));
  }
  chunks.push(Buffer.alloc(BLOCK * 2)); // end of archive
  return Buffer.concat(chunks);
}
