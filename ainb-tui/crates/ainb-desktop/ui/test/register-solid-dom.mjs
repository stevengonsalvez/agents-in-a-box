// `node --import` entry: registers the DOM `.tsx` loader before the test files load.
import { register } from "node:module";

register("./solid-dom.mjs", import.meta.url);
