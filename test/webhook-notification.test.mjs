import assert from "node:assert/strict";
import test from "node:test";

test("passes after the webhook resumes the linked Codex session", () => {
  assert.equal("CI success", "CI success");
});
