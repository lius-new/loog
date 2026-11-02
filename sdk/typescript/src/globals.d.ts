// The extension runtime provides these web globals; the type libraries for a
// bare ES2022 target do not declare them.
declare class TextEncoder {
  encode(input?: string): Uint8Array;
}
declare class TextDecoder {
  constructor(label?: string, options?: { fatal?: boolean });
  decode(input?: Uint8Array): string;
}
