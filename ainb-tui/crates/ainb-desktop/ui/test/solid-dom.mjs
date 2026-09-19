// Node module hooks that compile this window's `.tsx` components for the DOM,
// so a component test can mount one into a real document (happy-dom) and hold
// on to its nodes. The same two steps as `solid-ssr.mjs`: TypeScript stripped
// with the compiler the build uses (JSX kept), then the Solid preset, here in
// its `dom` mode, the mode the window itself is built in. Used only by
// `npm run test:dom`, which runs with the `browser` condition so `solid-js`
// resolves to its client build.

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { transformAsync } from "@babel/core";
import solid from "babel-preset-solid";
import ts from "typescript";

export async function load(url, context, nextLoad) {
  if (!url.endsWith(".tsx")) return nextLoad(url, context);
  const path = fileURLToPath(url);
  const source = await readFile(path, "utf8");
  const stripped = ts.transpileModule(source, {
    fileName: path,
    compilerOptions: {
      jsx: ts.JsxEmit.Preserve,
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2022,
      verbatimModuleSyntax: true,
    },
  }).outputText;
  const compiled = await transformAsync(stripped, {
    filename: path.replace(/\.tsx$/, ".jsx"),
    babelrc: false,
    configFile: false,
    presets: [[solid, { generate: "dom" }]],
  });
  return { format: "module", source: compiled.code, shortCircuit: true };
}
