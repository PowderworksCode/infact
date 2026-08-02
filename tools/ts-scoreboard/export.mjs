import { extract } from "./extract.mjs";
import fs from "node:fs";
// fixture file -> the rule it tests, and the APIs that rule says to reach for.
const RULES = {
  "prefer-find.test.ts":     { rule: "@typescript-eslint/prefer-find",  apis: ["ArrayFind"] },
  "prefer-array-find.js":    { rule: "unicorn/prefer-array-find",       apis: ["ArrayFind", "ArrayFindLast"] },
  "prefer-includes.test.ts": { rule: "@typescript-eslint/prefer-includes", apis: ["ArrayIncludes", "StringIncludes"] },
};
const out = [];
const messageIds = (c) => (c.errors ?? []).map((e) => e.messageId).filter(Boolean);

for (const [file, meta] of Object.entries(RULES)) {
  if (!fs.existsSync(file)) continue;
  const { valid, invalid } = extract(file);
  const cases = [
    ...valid.map((c) => ({ kind: "valid", c })),
    ...invalid.map((c) => ({ kind: "invalid", c })),
  ];
  for (const { kind, c } of cases) {
    out.push({ file, ...meta, kind, code: c.code, messageIds: messageIds(c) });
  }
}
fs.writeFileSync("cases.json", JSON.stringify(out, null, 1));
console.log(`${out.length} cases -> cases.json (${out.filter(c=>c.kind==="invalid").length} positives, ${out.filter(c=>c.kind==="valid").length} negatives)`);
