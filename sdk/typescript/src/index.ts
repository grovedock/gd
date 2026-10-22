// TypeScript/Node.js client for Grovedock (https://github.com/grovedock/gd).
// Read files, list directories, search and inspect history at any revision without cloning.
// Changeset and file contents are verified against their BLAKE3 ids.

import { fileURLToPath } from "node:url";
import path from "node:path";
import * as grpc from "@grpc/grpc-js";
import * as protoLoader from "@grpc/proto-loader";
import protobuf from "protobufjs";
import { blake3 } from "@noble/hashes/blake3.js";

const PROTO_DIR = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "proto");
const MAX_MESSAGE = 260 * 1024 * 1024;
const MODES = ["file", "exec", "symlink", "dir"] as const;
export type Mode = (typeof MODES)[number];

export class IntegrityError extends Error {}

export interface FileChange { path: string; blob: string | null; mode: Mode }
export interface Changeset {
  id: string; parents: string[]; author: string; message: string;
  timestampMs: number; changes: FileChange[]; redacted: boolean;
}
export interface DirEntry { name: string; mode: Mode; id: string | null }

const hex = (b: Uint8Array | Buffer | undefined): string => Buffer.from(b ?? []).toString("hex");
const idOf = (tag: number, data: Uint8Array): Buffer => {
  const input = new Uint8Array(data.length + 1);
  input[0] = tag;
  input.set(data, 1);
  return Buffer.from(blake3(input));
};
const mode = (m: number | string | undefined): Mode =>
  typeof m === "string" ? (m.toLowerCase() as Mode) : MODES[m ?? 0] ?? "file";

type Unary = (req: object, md: grpc.Metadata, cb: (e: grpc.ServiceError | null, r: any) => void) => void;

export class Client {
  #md = new grpc.Metadata();
  #svc: Record<string, any> = {};
  #objects: protobuf.Root;

  /** Connect to `url` ("http://host:7420" or "https://..."); `token` is optional. */
  constructor(url: string, token?: string) {
    const u = new URL(url);
    const creds = u.protocol === "https:" ? grpc.credentials.createSsl() : grpc.credentials.createInsecure();
    const def = protoLoader.loadSync(path.join(PROTO_DIR, "gd.proto"), {
      keepCase: true, longs: Number, enums: Number, defaults: true, includeDirs: [PROTO_DIR],
    });
    const v1 = (grpc.loadPackageDefinition(def) as any).gd.v1;
    const opts = { "grpc.max_receive_message_length": MAX_MESSAGE, "grpc.max_send_message_length": MAX_MESSAGE };
    for (const name of ["Browse", "History", "Changesets", "Diff", "Refs", "MergeBase", "Access"]) {
      this.#svc[name] = new v1[name](u.host, creds, opts);
    }
    if (token) this.#md.set("authorization", `Bearer ${token}`);
    this.#objects = protobuf.loadSync(path.join(PROTO_DIR, "gd_objects.proto"));
  }

  close(): void {
    for (const s of Object.values(this.#svc)) s.close();
  }

  #call<T>(service: string, method: string, req: object): Promise<T> {
    const fn: Unary = this.#svc[service][method].bind(this.#svc[service]);
    return new Promise((resolve, reject) => fn(req, this.#md, (e, r) => (e ? reject(e) : resolve(r))));
  }

  #changeset(info: any): Changeset {
    const encoded: Buffer = info.encoded;
    if (!info.redacted && !idOf(3, encoded).equals(info.id)) {
      throw new IntegrityError(`changeset ${hex(info.id)} does not match its content`);
    }
    const Cs = this.#objects.lookupType("gd.objects.v1.Changeset");
    const cs: any = Cs.toObject(Cs.decode(encoded), { longs: Number, enums: Number, defaults: true });
    return {
      id: hex(info.id),
      parents: cs.parents.map((p: Uint8Array) => hex(p)),
      author: cs.author,
      message: cs.message,
      timestampMs: cs.timestamp_ms ?? cs.timestampMs,
      changes: cs.changes.map((c: any) => ({ path: c.path, blob: hex(c.blob) || null, mode: mode(c.mode) })),
      redacted: info.redacted,
    };
  }

  async readFile(filePath: string, rev = "main"): Promise<Buffer> {
    const r: any = await this.#call("Browse", "ReadFile", { rev, path: filePath });
    if (!idOf(1, r.content).equals(r.id)) throw new IntegrityError(`${filePath} does not match its content id`);
    return r.content;
  }

  async listDir(dir = "", rev = "main"): Promise<DirEntry[]> {
    const r: any = await this.#call("Browse", "ListDir", { rev, path: dir });
    return r.entries.map((e: any) => ({ name: e.name, mode: mode(e.mode), id: hex(e.id) || null }));
  }

  async find(pattern: string, rev = "main", limit = 1000): Promise<string[]> {
    const r: any = await this.#call("Browse", "Find", { rev, pattern, limit });
    return r.paths;
  }

  log(rev = "main", filePath = "", limit = 50): Promise<Changeset[]> {
    return new Promise((resolve, reject) => {
      const out: Changeset[] = [];
      const stream = this.#svc.History.Log({ rev, path: filePath, limit }, this.#md);
      stream.on("data", (info: any) => {
        try { out.push(this.#changeset(info)); } catch (e) { stream.cancel(); reject(e); }
      });
      stream.on("error", reject);
      stream.on("end", () => resolve(out));
    });
  }

  async changeset(rev: string): Promise<Changeset> {
    return this.#changeset(await this.#call("Changesets", "Get", { rev }));
  }

  async diff(from: string, to: string): Promise<FileChange[]> {
    const r: any = await this.#call("Diff", "Diff", { from, to });
    return r.changes.map((c: any) => ({ path: c.path, blob: hex(c.blob) || null, mode: mode(c.mode) }));
  }

  async blame(filePath: string, rev = "main"): Promise<Array<{ changeset: string; text: string }>> {
    const r: any = await this.#call("History", "Blame", { rev, path: filePath });
    return r.lines.map((l: any) => ({ changeset: hex(l.changeset), text: l.text }));
  }

  async refs(): Promise<Record<string, string>> {
    const r: any = await this.#call("Refs", "List", {});
    return Object.fromEntries(r.refs.map((e: any) => [e.name, hex(e.id)]));
  }

  async mergeBase(a: string, b: string): Promise<string | null> {
    const r: any = await this.#call("MergeBase", "MergeBase", { a, b });
    return r.found ? hex(r.id) : null;
  }
}
