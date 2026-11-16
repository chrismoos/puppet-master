import ghosttyWasmUrl from "./vendor/ghostty-vt.wasm?url";
import ghosttyWritePtyWasmUrl from "./vendor/ghostty-write-pty.wasm?url&no-inline";
import { GHOSTTY_TERMINAL_OPTION } from "./abi";

type WasmFunction = (...args: Array<number | bigint>) => number;

interface TypeField {
  readonly offset: number;
  readonly size: number;
  readonly type: string;
}

interface TypeLayout {
  readonly underlying?: string;
  readonly type?: string;
  readonly size: number;
  readonly align: number;
  readonly fields: Readonly<Record<string, TypeField>>;
  readonly values?: Readonly<Record<string, number>>;
  readonly bits?: Readonly<Record<string, { readonly lsb: number; readonly width: number }>>;
}

type TypeLayouts = Readonly<Record<string, TypeLayout>>;

const textDecoder = new TextDecoder();

export class GhosttyRuntime {
  readonly memory: WebAssembly.Memory;
  readonly layouts: TypeLayouts;
  private readonly exports: WebAssembly.Exports;
  private memoryView: DataView;
  private readonly ptyWriters = new Map<number, (data: string) => void>();
  private nextPtyWriterId = 1;
  private writePtyFunctionIndex = 0;
  private snapshotReadFunctionIndex = 0;
  private nextReaderId = 1;
  private readers = new Map<number, (destination: Uint8Array) => number>();

  private constructor(instance: WebAssembly.Instance) {
    this.exports = instance.exports;
    const memory = instance.exports.memory;
    if (!(memory instanceof WebAssembly.Memory)) {
      throw new Error("libghostty-vt did not export WebAssembly memory");
    }
    this.memory = memory;
    this.memoryView = new DataView(memory.buffer);
    const jsonPointer = this.call("ghostty_type_json");
    const bytes = new Uint8Array(memory.buffer);
    let end = jsonPointer;
    while (end < bytes.length && bytes[end] !== 0) end += 1;
    const manifest = JSON.parse(textDecoder.decode(bytes.subarray(jsonPointer, end))) as { types: TypeLayouts };
    this.layouts = manifest.types;
  }

  static async load(): Promise<GhosttyRuntime> {
    const response = await fetch(ghosttyWasmUrl);
    if (!response.ok) {
      throw new Error(`Unable to load libghostty-vt (${response.status})`);
    }
    let instance: WebAssembly.Instance | undefined;
    const imports = {
      env: {
        log: (pointer: number, length: number) => {
          if (!instance) return;
          const memory = instance.exports.memory;
          if (!(memory instanceof WebAssembly.Memory)) return;
          const message = textDecoder.decode(new Uint8Array(memory.buffer, pointer, length));
          console.debug("[libghostty-vt]", message);
        },
      },
    };
    const result = await WebAssembly.instantiate(await response.arrayBuffer(), imports);
    instance = result.instance;
    const runtime = new GhosttyRuntime(result.instance);
    await runtime.installWritePtyTrampoline();
    return runtime;
  }

  call(name: string, ...args: Array<number | bigint>): number {
    const fn = this.exports[name];
    if (typeof fn !== "function") {
      throw new Error(`libghostty-vt export is unavailable: ${name}`);
    }
    return (fn as WasmFunction)(...args);
  }

  layout(name: string): TypeLayout {
    const layout = this.layouts[name];
    if (!layout) throw new Error(`libghostty-vt type layout is unavailable: ${name}`);
    return layout;
  }

  alloc(size: number): number {
    const pointer = this.call("ghostty_wasm_alloc", size);
    if (pointer === 0) throw new Error(`libghostty-vt failed to allocate ${size} bytes`);
    new Uint8Array(this.memory.buffer, pointer, size).fill(0);
    return pointer;
  }

  free(pointer: number, size: number): void {
    if (pointer !== 0) this.call("ghostty_wasm_free", pointer, size);
  }

  allocOpaque(): number {
    const pointer = this.call("ghostty_wasm_alloc_opaque");
    if (pointer === 0) throw new Error("libghostty-vt failed to allocate an opaque pointer");
    // Zero before *_new so partial-initialization cleanup cannot free a garbage pointer.
    new DataView(this.memory.buffer).setUint32(pointer, 0, true);
    return pointer;
  }

  freeOpaque(pointer: number): void {
    if (pointer !== 0) this.call("ghostty_wasm_free_opaque", pointer);
  }

  readPointer(slot: number): number {
    return this.memoryDataView().getUint32(slot, true);
  }

  attachPtyWriter(terminal: number, writer: (data: string) => void): number {
    if (this.writePtyFunctionIndex === 0) {
      throw new Error("libghostty-vt PTY callback trampoline is unavailable");
    }
    const id = this.nextPtyWriterId++;
    this.ptyWriters.set(id, writer);
    this.call("ghostty_terminal_set", terminal, GHOSTTY_TERMINAL_OPTION.userdata, id);
    this.call("ghostty_terminal_set", terminal, GHOSTTY_TERMINAL_OPTION.writePty, this.writePtyFunctionIndex);
    return id;
  }

  detachPtyWriter(terminal: number, id: number): void {
    this.call("ghostty_terminal_set", terminal, GHOSTTY_TERMINAL_OPTION.writePty, 0);
    this.call("ghostty_terminal_set", terminal, GHOSTTY_TERMINAL_OPTION.userdata, 0);
    this.ptyWriters.delete(id);
  }

  attachSnapshotReader(read: (destination: Uint8Array) => number): { pointer: number; dispose(): void } {
    const reader = this.layout("GhosttyReader");
    const pointer = this.alloc(reader.size);
    const id = this.nextReaderId++;
    this.readers.set(id, read);
    this.memoryDataView().setUint32(pointer + reader.fields.read.offset, this.snapshotReadFunctionIndex, true);
    this.memoryDataView().setUint32(pointer + reader.fields.userdata.offset, id, true);
    return { pointer, dispose: () => { this.readers.delete(id); this.free(pointer, reader.size); } };
  }

  view(pointer: number, size?: number): DataView {
    return new DataView(this.memory.buffer, pointer, size);
  }

  bytes(pointer: number, size: number): Uint8Array {
    return new Uint8Array(this.memory.buffer, pointer, size);
  }

  memoryDataView(): DataView {
    if (this.memoryView.buffer !== this.memory.buffer) {
      this.memoryView = new DataView(this.memory.buffer);
    }
    return this.memoryView;
  }

  /** Zeroes a struct and records its size when libghostty-vt versions it with a `size` field. */
  initStruct(pointer: number, structName: string): void {
    const layout = this.layout(structName);
    this.bytes(pointer, layout.size).fill(0);
    if (layout.fields.size) this.setField(pointer, structName, "size", layout.size);
  }

  setField(pointer: number, structName: string, fieldName: string, value: number): void {
    const field = this.layout(structName).fields[fieldName];
    if (!field) throw new Error(`libghostty-vt field is unavailable: ${structName}.${fieldName}`);
    const view = this.memoryDataView();
    const offset = pointer + field.offset;
    switch (this.layouts[field.type]?.underlying ?? this.layouts[field.type]?.type ?? field.type) {
      case "bool":
      case "u8":
        view.setUint8(offset, value);
        return;
      case "u16":
        view.setUint16(offset, value, true);
        return;
      case "i32":
        view.setInt32(offset, value, true);
        return;
      case "u32":
      case "enum":
      case "pointer":
        view.setUint32(offset, value, true);
        return;
      case "u64":
        view.setBigUint64(offset, BigInt(value), true);
        return;
      default:
        throw new Error(`Unsupported libghostty-vt field type: ${field.type}`);
    }
  }

  /** Reads one bit range of a packed field, such as a style flag. */
  readBits(pointer: number, structName: string, fieldName: string, bitName: string): number {
    const field = this.layout(structName).fields[fieldName];
    const bits = field && this.layouts[field.type]?.bits?.[bitName];
    if (!field || !bits) throw new Error(`libghostty-vt bits are unavailable: ${structName}.${fieldName}.${bitName}`);
    const word = this.readField(pointer, structName, fieldName);
    return (word >>> bits.lsb) & ((1 << bits.width) - 1);
  }

  readField(pointer: number, structName: string, fieldName: string): number {
    const field = this.layout(structName).fields[fieldName];
    if (!field) throw new Error(`libghostty-vt field is unavailable: ${structName}.${fieldName}`);
    const view = this.memoryDataView();
    const offset = pointer + field.offset;
    switch (this.layouts[field.type]?.underlying ?? this.layouts[field.type]?.type ?? field.type) {
      case "bool":
      case "u8":
        return view.getUint8(offset);
      case "u16":
        return view.getUint16(offset, true);
      case "i32":
        return view.getInt32(offset, true);
      case "u32":
      case "enum":
      case "pointer":
        return view.getUint32(offset, true);
      case "u64":
        return Number(view.getBigUint64(offset, true));
      default:
        throw new Error(`Unsupported libghostty-vt field type: ${field.type}`);
    }
  }

  private async installWritePtyTrampoline(): Promise<void> {
    const response = await fetch(ghosttyWritePtyWasmUrl);
    if (!response.ok) {
      throw new Error(`Unable to load the libghostty-vt PTY trampoline (${response.status})`);
    }
    const result = await WebAssembly.instantiate(await response.arrayBuffer(), {
      env: {
        t3_snapshot_read: (userdata: number, pointer: number, capacity: number, outRead: number) => {
          const read = this.readers.get(userdata);
          if (!read) return 0;
          try {
            const count = read(this.bytes(pointer, capacity));
            this.memoryDataView().setUint32(outRead, count, true);
            return 1;
          } catch { return 0; }
        },
        t3_write_pty: (_terminal: number, userdata: number, pointer: number, length: number) => {
          const writer = this.ptyWriters.get(userdata);
          if (!writer || length === 0) return;
          writer(textDecoder.decode(new Uint8Array(this.memory.buffer, pointer, length)));
        },
      },
    });
    const trampoline = result.instance.exports.ghostty_write_pty;
    const table = this.exports.__indirect_function_table;
    if (typeof trampoline !== "function" || !(table instanceof WebAssembly.Table)) {
      throw new Error("libghostty-vt did not expose its callback table");
    }
    const index = table.length;
    // WebKit grow(1, fn) stores broken type info and traps on call_indirect; table.set canonicalizes it.
    table.grow(1);
    table.set(index, trampoline);
    this.writePtyFunctionIndex = index;
    const reader = result.instance.exports.ghostty_snapshot_read;
    if (typeof reader !== "function") throw new Error("Snapshot reader trampoline is unavailable");
    const readerIndex = table.length;
    table.grow(1);
    table.set(readerIndex, reader);
    this.snapshotReadFunctionIndex = readerIndex;
  }
}

let runtimePromise: Promise<GhosttyRuntime> | null = null;

export function loadGhosttyRuntime(): Promise<GhosttyRuntime> {
  runtimePromise ??= GhosttyRuntime.load().catch((error) => {
    runtimePromise = null;
    throw error;
  });
  return runtimePromise;
}
