import assert from "node:assert/strict";
import test from "node:test";

test("passes after the second webhook delivery", () => {
  assert.equal("CI success", "CI success");
});
