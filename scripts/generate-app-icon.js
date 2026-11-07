#!/usr/bin/env node

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');

const DEFAULT_SIZES = [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 128, 256];
const PNG_SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const LANCZOS_RADIUS = 3;

const crcTable = new Uint32Array(256);
for (let index = 0; index < crcTable.length; index += 1) {
  let value = index;
  for (let bit = 0; bit < 8; bit += 1) {
    value = (value & 1) !== 0
      ? 0xedb88320 ^ (value >>> 1)
      : value >>> 1;
  }
  crcTable[index] = value >>> 0;
}

function crc32(buffer) {
  let crc = 0xffffffff;
  for (const byte of buffer) {
    crc = crcTable[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function parseArguments(argv) {
  const repositoryRoot = path.resolve(__dirname, '..');
  const options = {
    source: path.join(repositoryRoot, 'icons', 'source', 'app-icon-transparent-1024.png'),
    output: path.join(repositoryRoot, 'icons', 'app.ico'),
    sizes: DEFAULT_SIZES,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const value = argv[index + 1];

    if (argument === '--help' || argument === '-h') {
      console.log([
        'Usage: node scripts/generate-app-icon.js [options]',
        '',
        'Options:',
        '  --source <path>       Source RGBA PNG file',
        '  --output <path>       Output ICO file',
        '  --sizes <list>        Comma-separated sizes from 1 to 256',
        '  -h, --help            Show this help',
      ].join('\n'));
      process.exit(0);
    }

    if (!['--source', '--output', '--sizes'].includes(argument)) {
      throw new Error(`Unknown argument: ${argument}`);
    }
    if (value === undefined) {
      throw new Error(`Missing value for ${argument}`);
    }

    if (argument === '--source') {
      options.source = path.resolve(value);
    } else if (argument === '--output') {
      options.output = path.resolve(value);
    } else {
      options.sizes = value.split(',').map((item) => Number(item.trim()));
    }
    index += 1;
  }

  options.sizes = [...new Set(options.sizes)].sort((left, right) => left - right);
  if (options.sizes.length === 0) {
    throw new Error('At least one icon size is required.');
  }
  for (const size of options.sizes) {
    if (!Number.isInteger(size) || size < 1 || size > 256) {
      throw new Error(`ICO dimensions must be integers between 1 and 256 pixels: ${size}`);
    }
  }

  return options;
}

function readPng(filePath) {
  const png = fs.readFileSync(filePath);
  if (png.length < PNG_SIGNATURE.length || !png.subarray(0, 8).equals(PNG_SIGNATURE)) {
    throw new Error(`Not a PNG file: ${filePath}`);
  }

  let width;
  let height;
  let bitDepth;
  let colorType;
  let compressionMethod;
  let filterMethod;
  let interlaceMethod;
  const idatChunks = [];
  let offset = PNG_SIGNATURE.length;

  while (offset < png.length) {
    if (offset + 12 > png.length) {
      throw new Error('The PNG contains a truncated chunk.');
    }

    const length = png.readUInt32BE(offset);
    const type = png.toString('ascii', offset + 4, offset + 8);
    const dataStart = offset + 8;
    const dataEnd = dataStart + length;
    const chunkEnd = dataEnd + 4;
    if (chunkEnd > png.length) {
      throw new Error(`The PNG ${type} chunk is truncated.`);
    }

    const storedCrc = png.readUInt32BE(dataEnd);
    const calculatedCrc = crc32(png.subarray(offset + 4, dataEnd));
    if (storedCrc !== calculatedCrc) {
      throw new Error(`The PNG ${type} chunk failed its CRC check.`);
    }

    const data = png.subarray(dataStart, dataEnd);
    if (type === 'IHDR') {
      if (length !== 13) {
        throw new Error('The PNG IHDR chunk has an invalid size.');
      }
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      bitDepth = data[8];
      colorType = data[9];
      compressionMethod = data[10];
      filterMethod = data[11];
      interlaceMethod = data[12];
    } else if (type === 'IDAT') {
      idatChunks.push(data);
    } else if (type === 'IEND') {
      break;
    }

    offset = chunkEnd;
  }

  if (!width || !height || idatChunks.length === 0) {
    throw new Error('The PNG is missing required image data.');
  }
  if (bitDepth !== 8 || colorType !== 6) {
    throw new Error(
      `The source PNG must use 8-bit RGBA pixels (got bit depth ${bitDepth}, color type ${colorType}).`,
    );
  }
  if (compressionMethod !== 0 || filterMethod !== 0 || interlaceMethod !== 0) {
    throw new Error('The source PNG must use standard compression and filtering without interlacing.');
  }

  const bytesPerPixel = 4;
  const stride = width * bytesPerPixel;
  const inflated = zlib.inflateSync(Buffer.concat(idatChunks));
  const expectedLength = height * (stride + 1);
  if (inflated.length !== expectedLength) {
    throw new Error(`Unexpected PNG image-data size: ${inflated.length}; expected ${expectedLength}.`);
  }

  const pixels = Buffer.alloc(width * height * bytesPerPixel);
  let inputOffset = 0;
  let previousRow = Buffer.alloc(stride);

  for (let y = 0; y < height; y += 1) {
    const filter = inflated[inputOffset];
    inputOffset += 1;
    const row = Buffer.from(inflated.subarray(inputOffset, inputOffset + stride));
    inputOffset += stride;

    for (let x = 0; x < stride; x += 1) {
      const left = x >= bytesPerPixel ? row[x - bytesPerPixel] : 0;
      const above = previousRow[x];
      const upperLeft = x >= bytesPerPixel ? previousRow[x - bytesPerPixel] : 0;

      if (filter === 1) {
        row[x] = (row[x] + left) & 0xff;
      } else if (filter === 2) {
        row[x] = (row[x] + above) & 0xff;
      } else if (filter === 3) {
        row[x] = (row[x] + Math.floor((left + above) / 2)) & 0xff;
      } else if (filter === 4) {
        row[x] = (row[x] + paethPredictor(left, above, upperLeft)) & 0xff;
      } else if (filter !== 0) {
        throw new Error(`Unsupported PNG filter type: ${filter}`);
      }
    }

    row.copy(pixels, y * stride);
    previousRow = row;
  }

  return { width, height, pixels };
}

function paethPredictor(left, above, upperLeft) {
  const prediction = left + above - upperLeft;
  const distanceLeft = Math.abs(prediction - left);
  const distanceAbove = Math.abs(prediction - above);
  const distanceUpperLeft = Math.abs(prediction - upperLeft);

  if (distanceLeft <= distanceAbove && distanceLeft <= distanceUpperLeft) {
    return left;
  }
  return distanceAbove <= distanceUpperLeft ? above : upperLeft;
}

function sinc(value) {
  if (Math.abs(value) < Number.EPSILON) {
    return 1;
  }
  const angle = Math.PI * value;
  return Math.sin(angle) / angle;
}

function lanczos(value) {
  const absolute = Math.abs(value);
  if (absolute >= LANCZOS_RADIUS) {
    return 0;
  }
  return sinc(value) * sinc(value / LANCZOS_RADIUS);
}

function clampByte(value) {
  return Math.max(0, Math.min(255, Math.round(value)));
}

function resizeRgba(source, targetWidth, targetHeight) {
  if (source.width === targetWidth && source.height === targetHeight) {
    return Buffer.from(source.pixels);
  }

  const target = Buffer.alloc(targetWidth * targetHeight * 4);
  const scaleX = source.width / targetWidth;
  const scaleY = source.height / targetHeight;
  const filterScaleX = Math.max(1, scaleX);
  const filterScaleY = Math.max(1, scaleY);
  const supportX = LANCZOS_RADIUS * filterScaleX;
  const supportY = LANCZOS_RADIUS * filterScaleY;

  for (let targetY = 0; targetY < targetHeight; targetY += 1) {
    const sourceY = (targetY + 0.5) * scaleY - 0.5;
    const startY = Math.max(0, Math.ceil(sourceY - supportY));
    const endY = Math.min(source.height - 1, Math.floor(sourceY + supportY));

    for (let targetX = 0; targetX < targetWidth; targetX += 1) {
      const sourceX = (targetX + 0.5) * scaleX - 0.5;
      const startX = Math.max(0, Math.ceil(sourceX - supportX));
      const endX = Math.min(source.width - 1, Math.floor(sourceX + supportX));
      let totalWeight = 0;
      let alphaSum = 0;
      let redSum = 0;
      let greenSum = 0;
      let blueSum = 0;

      for (let y = startY; y <= endY; y += 1) {
        const weightY = lanczos((y - sourceY) / filterScaleY);
        for (let x = startX; x <= endX; x += 1) {
          const weight = weightY * lanczos((x - sourceX) / filterScaleX);
          if (weight === 0) {
            continue;
          }

          const sourceOffset = (y * source.width + x) * 4;
          const alpha = source.pixels[sourceOffset + 3] / 255;
          totalWeight += weight;
          alphaSum += alpha * weight;
          redSum += source.pixels[sourceOffset] * alpha * weight;
          greenSum += source.pixels[sourceOffset + 1] * alpha * weight;
          blueSum += source.pixels[sourceOffset + 2] * alpha * weight;
        }
      }

      const targetOffset = (targetY * targetWidth + targetX) * 4;
      const normalizedAlpha = totalWeight === 0 ? 0 : alphaSum / totalWeight;
      if (normalizedAlpha <= 1e-8 || alphaSum <= 1e-8) {
        target.fill(0, targetOffset, targetOffset + 4);
      } else {
        target[targetOffset] = clampByte(redSum / alphaSum);
        target[targetOffset + 1] = clampByte(greenSum / alphaSum);
        target[targetOffset + 2] = clampByte(blueSum / alphaSum);
        target[targetOffset + 3] = clampByte(normalizedAlpha * 255);
      }
    }
  }

  return target;
}

function createPngChunk(type, data) {
  const typeBuffer = Buffer.from(type, 'ascii');
  const lengthBuffer = Buffer.alloc(4);
  lengthBuffer.writeUInt32BE(data.length);
  const crcBuffer = Buffer.alloc(4);
  crcBuffer.writeUInt32BE(crc32(Buffer.concat([typeBuffer, data])));
  return Buffer.concat([lengthBuffer, typeBuffer, data, crcBuffer]);
}

function encodePng(width, height, pixels) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;

  const stride = width * 4;
  const scanlines = Buffer.alloc(height * (stride + 1));
  for (let y = 0; y < height; y += 1) {
    const destinationOffset = y * (stride + 1);
    scanlines[destinationOffset] = 0;
    pixels.copy(scanlines, destinationOffset + 1, y * stride, (y + 1) * stride);
  }

  return Buffer.concat([
    PNG_SIGNATURE,
    createPngChunk('IHDR', ihdr),
    createPngChunk('IDAT', zlib.deflateSync(scanlines, { level: 9 })),
    createPngChunk('IEND', Buffer.alloc(0)),
  ]);
}

function encodeIco(frames) {
  const directory = Buffer.alloc(6 + frames.length * 16);
  directory.writeUInt16LE(0, 0);
  directory.writeUInt16LE(1, 2);
  directory.writeUInt16LE(frames.length, 4);

  let imageOffset = directory.length;
  frames.forEach((frame, index) => {
    const entryOffset = 6 + index * 16;
    const encodedSize = frame.size === 256 ? 0 : frame.size;
    directory[entryOffset] = encodedSize;
    directory[entryOffset + 1] = encodedSize;
    directory[entryOffset + 2] = 0;
    directory[entryOffset + 3] = 0;
    directory.writeUInt16LE(1, entryOffset + 4);
    directory.writeUInt16LE(32, entryOffset + 6);
    directory.writeUInt32LE(frame.png.length, entryOffset + 8);
    directory.writeUInt32LE(imageOffset, entryOffset + 12);
    imageOffset += frame.png.length;
  });

  return Buffer.concat([directory, ...frames.map((frame) => frame.png)]);
}

function main() {
  const options = parseArguments(process.argv.slice(2));
  const sourcePath = path.resolve(options.source);
  const outputPath = path.resolve(options.output);
  const source = readPng(sourcePath);

  if (source.width !== source.height) {
    throw new Error(`The source artwork must be square, got ${source.width}x${source.height}.`);
  }
  const largestSize = Math.max(...options.sizes);
  if (source.width < largestSize) {
    throw new Error('The source artwork is smaller than the largest requested icon size.');
  }

  const frames = options.sizes.map((size) => ({
    size,
    png: encodePng(size, size, resizeRgba(source, size, size)),
  }));

  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.writeFileSync(outputPath, encodeIco(frames));
  console.log(`Generated ${outputPath} with sizes: ${options.sizes.join(', ')}`);
}

module.exports = { encodeIco };

if (require.main === module) {
  try {
    main();
  } catch (error) {
    console.error(`Failed to generate app icon: ${error.message}`);
    process.exitCode = 1;
  }
}
