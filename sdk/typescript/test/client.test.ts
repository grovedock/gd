// Runs against a seeded server: eval "$(../test-server.sh)" (sets GD_URL).
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client } from "../src/index.ts";

const url = process.env.GD_URL;

test("reads, history, diff, blame against a live server", { skip: !url && "GD_URL not set" }, async () => {
  const c = new Client(url!);
  try {
    assert.equal((await c.readFile("src/lib.rs")).toString(), "pub fn answer() -> u32 { 42 }\n");
    assert.deepEqual((await c.listDir()).map((e) => e.name), ["README.md", "docs", "src"]);
    assert.deepEqual(await c.find("guide"), ["docs/guide.md"]);
    const log = await c.log();
    assert.deepEqual(log.map((x) => x.message), ["fix the answer", "initial"]);
    assert.equal(log[0].parents[0], log[1].id);
    assert.equal(log[0].author, "SDK Test <sdk@example.com>");
    assert.deepEqual(new Set((await c.diff(log[1].id, log[0].id)).map((x) => x.path)), new Set(["src/lib.rs", "docs/guide.md"]));
    assert.deepEqual(await c.blame("src/lib.rs"), [{ changeset: log[0].id, text: "pub fn answer() -> u32 { 42 }" }]);
    assert.deepEqual(await c.refs(), { main: log[0].id });
    assert.equal(await c.mergeBase(log[1].id, log[0].id), log[1].id);
    assert.equal((await c.changeset(log[1].id)).message, "initial");
  } finally {
    c.close();
  }
});
