// `node --import` entry: registers the `.tsx` loader before the test files load.
import { register } from "node:module";

register("./solid-ssr.mjs", import.meta.url);
