import assert from "node:assert/strict";
import test from "node:test";

test("intentionally fails to verify the linked PR session", () => {
  assert.fail("intentional failure to verify the linked PR session");
});
