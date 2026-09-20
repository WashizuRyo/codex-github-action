import assert from "node:assert/strict";
import test from "node:test";

test("intentionally fails to verify the CI failure workflow", () => {
  assert.fail("intentional failure to verify the linked session");
});
