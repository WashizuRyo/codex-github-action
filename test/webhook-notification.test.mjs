import assert from "node:assert/strict";
import test from "node:test";

test("fails CI so the webhook resumes the linked Codex session again", () => {
  assert.equal("CI failure", "CI success");
});
