/* tslint:disable */
/* eslint-disable */

/**
 * Parse a full AsciiDoc document.
 *
 * Returns a JS object:
 * - `{ ok: true, value: <Document ASG>, warnings: [...] }` on success
 * - `{ ok: false, error: <message> }` on parse failure
 *
 * `safe_mode` accepts `"unsafe"` (default), `"safe"`, `"server"`, `"secure"`.
 * The runtime built-in attributes (`safe-mode-level`, `asciidoctor-version`,
 * `backend`, `doctype`, etc.) are injected automatically by the parser.
 */
export function parse_block(source: string, safe_mode?: string | null): any;

/**
 * Parse a fragment of inline AsciiDoc and return the inline nodes.
 *
 * Same envelope as `parse_block`. Every returned `InlineNode` variant
 * carries a `location` field with `{ line, col }` ranges — useful when the
 * caller already has block-level structure and wants char-level offsets
 * inside one block's source text.
 */
export function parse_inline(source: string, safe_mode?: string | null): any;

/**
 * Return the underlying `acdc-parser` version.
 */
export function version(): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly parse_block: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly parse_inline: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly version: (a: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number, b: number, c: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
