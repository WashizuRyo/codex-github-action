import assert from "node:assert/strict";
import test from "node:test";

test("fails CI for a second webhook delivery", () => {
  assert.equal("CI failure again", "CI success");
});
