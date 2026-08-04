// Extract (code, expected-positive) pairs from ESLint RuleTester fixtures.
//
// The test files are TypeScript, so they are parsed with the TypeScript
// compiler rather than pattern-matched. `invalid: [...]` entries are the
// annotated positives; `valid: [...]` entries are annotated NEGATIVES, which
// clippy's ui tests do not give us and which are worth just as much — a
// finding on a valid case is a false positive with ground truth attached.
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

// Where the TypeScript compiler is, without naming one machine.
//
// This module lives in the repository and the corpus it reads lives outside it,
// so a bare `import "typescript"` resolves from the wrong place: ESM looks
// beside the importing FILE, and the install is beside the CORPUS. Resolving
// from the working directory and from <measure>/ts-lints is what makes
// `node tools/ts-scoreboard/export.mjs` work from either. An explicit path wins
// over both, which is the same override the scorer takes.
//
// This was a hardcoded absolute path into one checkout, which made the harness
// unrunnable anywhere else — the same failure the other two harnesses had
// before their paths were derived from the repository.
// Where the rule tests and the generated cases.json live.
//
// Derived from this file's location, the same way the two Python harnesses
// derive theirs, so a run does not depend on which directory it started in.
export const corpus = path.join(
  process.env.INFACT_MEASURE ?? path.join(import.meta.dirname, "../../../measure"),
  "ts-lints",
);

function loadTypeScript() {
  const flag = process.argv.indexOf("--typescript");
  const explicit =
    (flag >= 0 ? process.argv[flag + 1] : undefined) ?? process.env.TYPESCRIPT;
  const tried = [];
  const candidates = [
    ...(explicit ? [explicit] : []),
    "typescript",
  ];
  const bases = [process.cwd(), corpus, import.meta.dirname];
  for (const base of bases) {
    // a directory is not a module, so resolution starts from a file inside it
    const require = createRequire(pathToFileURL(path.join(base, "-")));
    for (const candidate of candidates) {
      try {
        return require(candidate);
      } catch {
        tried.push(`${candidate} from ${base}`);
      }
    }
  }
  throw new Error(
    `no TypeScript compiler found. Install one beside the corpus\n` +
      `  (cd <measure>/ts-lints && npm install typescript@5)\n` +
      `or pass --typescript <path to typescript.js>. Tried:\n  ` +
      tried.join("\n  "),
  );
}

const ts = loadTypeScript();

function text(node, sf) {
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  // noFormat`...` and plain template literals
  if (ts.isTaggedTemplateExpression(node)) return text(node.template, sf);
  if (ts.isTemplateExpression(node)) return null; // has substitutions; skip
  return null;
}

export function extract(file) {
  const src = fs.readFileSync(file, "utf8");
  const sf = ts.createSourceFile(file, src, ts.ScriptTarget.ES2022, true);
  const out = { valid: [], invalid: [] };

  function readCase(el, kind) {
    if (kind === "valid") {
      const code = text(el, sf);
      if (code !== null) return { code, errors: [] };
      if (ts.isObjectLiteralExpression(el)) {
        const c = el.properties.find(p => p.name?.getText(sf) === "code");
        const code = c && text(c.initializer, sf);
        if (code != null) return { code, errors: [] };
      }
      return null;
    }
    if (!ts.isObjectLiteralExpression(el)) return null;
    const codeProp = el.properties.find(p => p.name?.getText(sf) === "code");
    const errProp = el.properties.find(p => p.name?.getText(sf) === "errors");
    const code = codeProp && text(codeProp.initializer, sf);
    if (code == null) return null;
    const errors = [];
    if (errProp && ts.isArrayLiteralExpression(errProp.initializer)) {
      for (const e of errProp.initializer.elements) {
        if (!ts.isObjectLiteralExpression(e)) continue;
        const get = n => {
          const p = e.properties.find(p => p.name?.getText(sf) === n);
          if (!p) return undefined;
          const i = p.initializer;
          if (ts.isNumericLiteral(i)) return Number(i.text);
          if (ts.isStringLiteral(i)) return i.text;
          return undefined;
        };
        errors.push({ line: get("line"), column: get("column"), messageId: get("messageId") });
      }
    }
    return { code, errors };
  }

  function walk(n) {
    if (ts.isPropertyAssignment(n) && ["valid", "invalid"].includes(n.name.getText(sf))
        && ts.isArrayLiteralExpression(n.initializer)) {
      const kind = n.name.getText(sf);
      for (const el of n.initializer.elements) {
        const c = readCase(el, kind);
        if (c) out[kind].push(c);
      }
      return;
    }
    ts.forEachChild(n, walk);
  }
  walk(sf);
  return out;
}

// --- shape classification: which cases can the laws reach today?
const LOOP = /\b(for\s*\(|while\s*\(|for\s+(const|let|var)\s)/;
const CHAIN = /\.\s*(filter|map|find|some|every|reduce|flat|indexOf|includes)\s*\(/;

if (process.argv[1].endsWith("extract.mjs")) {
  let tv = 0, ti = 0, tpos = 0;
  console.log(
    "fixture".padEnd(28), "valid", "invalid", "positives", " loop", "chain", "both");
  for (const f of process.argv.slice(2)) {
    const { valid, invalid } = extract(f);
    const pos = invalid.reduce((a, c) => a + Math.max(c.errors.length, 1), 0);
    let loop = 0, chain = 0, both = 0;
    for (const c of invalid) {
      const l = LOOP.test(c.code), h = CHAIN.test(c.code);
      if (l && h) both++; else if (l) loop++; else if (h) chain++;
    }
    tv += valid.length; ti += invalid.length; tpos += pos;
    console.log(
      f.split("/").pop().padEnd(28),
      String(valid.length).padStart(5), String(invalid.length).padStart(7),
      String(pos).padStart(9), String(loop).padStart(5),
      String(chain).padStart(5), String(both).padStart(4));
  }
  console.log("\nTOTAL".padEnd(28), String(tv).padStart(5), String(ti).padStart(7), String(tpos).padStart(9));
}
