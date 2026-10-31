// Offsets in the contract are UTF-8 byte offsets of the snapshot; JavaScript
// strings are indexed in UTF-16 code units. These helpers convert between the
// two without ever producing an offset inside a character.

const encoder = new TextEncoder();

/** The number of bytes `text` takes in UTF-8. */
export function utf8Length(text: string): number {
  return encoder.encode(text).length;
}

/** The UTF-8 byte offset of the UTF-16 index `index` in `text`. */
export function byteOffset(text: string, index: number): number {
  return utf8Length(text.slice(0, Math.max(0, Math.min(index, text.length))));
}

/**
 * The UTF-16 index of the UTF-8 byte offset `offset` in `text`. An offset inside
 * a character rounds down to the start of that character; one past the end is
 * the end of the text.
 */
export function stringIndex(text: string, offset: number): number {
  if (offset <= 0) return 0;
  let bytes = 0;
  let index = 0;
  while (index < text.length) {
    const point = text.codePointAt(index) as number;
    const size = point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4;
    if (bytes + size > offset) return index;
    bytes += size;
    index += point > 0xffff ? 2 : 1;
  }
  return text.length;
}

/** A byte range of the contract, from plain numbers. */
export function byteRange(start: number, end: number): { start: bigint; end: bigint } {
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end < start) {
    throw new RangeError(`not a byte range: ${start}..${end}`);
  }
  return { start: BigInt(start), end: BigInt(end) };
}

/** Both ends of a contract range as numbers (ranges of a snapshot are far below 2^53). */
export function bounds(range: { start: bigint; end: bigint }): [number, number] {
  return [Number(range.start), Number(range.end)];
}

/**
 * The text of `range` out of the whole snapshot text `text`, where the range is
 * in UTF-8 bytes.
 */
export function slice(text: string, range: { start: bigint; end: bigint }): string {
  const [start, end] = bounds(range);
  return text.slice(stringIndex(text, start), stringIndex(text, end));
}
