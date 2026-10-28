const OSC = 0x5d;
const CSI = 0x5b;
const DCS = 0x50;
const SOS = 0x58;
const PM = 0x5e;
const APC = 0x5f;
const BEL = 0x07;
const ESC = 0x1b;
const ST = 0x5c;
const CAN = 0x18;
const SUB = 0x1a;
const MAX_OSC_BYTES = 64 * 1024;

export class OscObserver {
  private state: "ground" | "escape" | "csi" | "osc" | "osc-escape" | "string" | "string-escape" = "ground";
  private payload: number[] = [];
  private overflow = false;
  private readonly decoder = new TextDecoder();

  constructor(private readonly onOsc: (payload: string) => void) {}

  write(data: string | Uint8Array): void {
    const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
    for (const byte of bytes) this.consume(byte);
  }

  reset(): void {
    this.state = "ground";
    this.payload = [];
    this.overflow = false;
  }

  private consume(byte: number): void {
    switch (this.state) {
      case "ground":
        if (byte === ESC) this.state = "escape";
        return;
      case "escape":
        if (byte === OSC) this.start("osc");
        else if (byte === CSI) this.state = "csi";
        else if (byte === DCS || byte === SOS || byte === PM || byte === APC) this.state = "string";
        else this.state = byte === ESC ? "escape" : "ground";
        return;
      case "csi":
        if (byte >= 0x40 && byte <= 0x7e) this.state = "ground";
        else if (byte === ESC) this.state = "escape";
        return;
      case "osc":
        if (byte === BEL) this.finish();
        else if (byte === ESC) this.state = "osc-escape";
        else if (byte === CAN || byte === SUB) this.reset();
        else this.push(byte);
        return;
      case "osc-escape":
        if (byte === ST) this.finish();
        else {
          this.push(ESC);
          if (byte === ESC) this.state = "osc-escape";
          else {
            this.push(byte);
            this.state = "osc";
          }
        }
        return;
      case "string":
        if (byte === ESC) this.state = "string-escape";
        else if (byte === CAN || byte === SUB) this.state = "ground";
        return;
      case "string-escape":
        this.state = byte === ST || byte === CAN || byte === SUB ? "ground" : byte === ESC ? "string-escape" : "string";
        return;
    }
  }

  private start(state: "osc"): void {
    this.payload = [];
    this.overflow = false;
    this.state = state;
  }

  private push(byte: number): void {
    if (this.payload.length >= MAX_OSC_BYTES) {
      this.overflow = true;
      return;
    }
    this.payload.push(byte);
  }

  private finish(): void {
    if (!this.overflow) this.onOsc(this.decoder.decode(new Uint8Array(this.payload)));
    this.reset();
  }
}
