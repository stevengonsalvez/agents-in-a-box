// Node module hooks that compile this window's `.tsx` components for a server
// render, so the DOM half of parity can mount `SettingsPage` under the node
// test runner: TypeScript is stripped with the compiler the build uses (JSX
// kept), then the Solid preset turns the JSX into the SSR runtime's calls.
// `.ts` files still go through node's own type stripping. Used only by
// `npm run test:parity`, which runs without the `browser` condition so
// `solid-js/web` resolves to its server build.

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
    presets: [[solid, { generate: "ssr", hydratable: false }]],
  });
  return { format: "module", source: compiled.code, shortCircuit: true };
}
