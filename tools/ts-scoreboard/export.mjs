import { extract, corpus } from "./extract.mjs";
import fs from "node:fs";
import path from "node:path";
// fixture file -> the rule it tests, and the APIs that rule says to reach for.
const RULES = {
  "prefer-find.test.ts":     { rule: "@typescript-eslint/prefer-find",  apis: ["ArrayFind"] },
  "prefer-array-find.js":    { rule: "unicorn/prefer-array-find",       apis: ["ArrayFind", "ArrayFindLast"] },
  "prefer-includes.test.ts": { rule: "@typescript-eslint/prefer-includes", apis: ["ArrayIncludes", "StringIncludes"] },
};
const out = [];
const missing = [];
const messageIds = (c) => (c.errors ?? []).map((e) => e.messageId).filter(Boolean);

for (const [file, meta] of Object.entries(RULES)) {
  const source = path.join(corpus, file);
  // a rule file that was never fetched is not a rule file with no cases, and a
  // total that silently drops 98 positives looks exactly like a working run
  if (!fs.existsSync(source)) {
    missing.push(source);
    continue;
  }
  const { valid, invalid } = extract(source);
  const cases = [
    ...valid.map((c) => ({ kind: "valid", c })),
    ...invalid.map((c) => ({ kind: "invalid", c })),
  ];
  for (const { kind, c } of cases) {
    out.push({ file, ...meta, kind, code: c.code, messageIds: messageIds(c) });
  }
}
const cases = path.join(corpus, "cases.json");
fs.writeFileSync(cases, JSON.stringify(out, null, 1));
console.log(`${out.length} cases -> ${cases} (${out.filter(c=>c.kind==="invalid").length} positives, ${out.filter(c=>c.kind==="valid").length} negatives)`);
if (missing.length) {
  console.error(`NOT FETCHED, so their cases are absent from the score:\n  ${missing.join("\n  ")}`);
  process.exitCode = 1;
}
