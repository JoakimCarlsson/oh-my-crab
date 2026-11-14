/**
 * The JavaScript kernel of the eval tool.
 *
 * Reads one JSON request per line on standard input, runs its code in one
 * context kept for the life of the process, and answers with one line that
 * starts with the marker in OMC_EVAL_MARKER, followed by JSON holding
 * the captured output, the value of the last expression and any error.
 */

"use strict";

const readline = require("node:readline");
const util = require("node:util");
const vm = require("node:vm");

const MARKER = process.env.OMC_EVAL_MARKER;
const captured = [];

/**
 * Records a console call in the output of the request being run.
 *
 * @param {unknown[]} args - The values passed to the console method.
 * @returns {void}
 */
function record(...args) {
  captured.push(`${util.format(...args)}\n`);
}

const sandbox = {
  console: {
    log: record,
    info: record,
    warn: record,
    error: record,
    debug: record,
    dir: (value) => record(util.inspect(value)),
  },
  require,
  process,
  Buffer,
  URL,
  URLSearchParams,
  TextEncoder,
  TextDecoder,
  setTimeout,
  clearTimeout,
  setInterval,
  clearInterval,
  setImmediate,
  clearImmediate,
  queueMicrotask,
  structuredClone,
  fetch: globalThis.fetch,
};
const context = vm.createContext(sandbox);

/**
 * Compiles code as a script, or, when it uses top-level await, as an async
 * arrow function returning it when it is one expression and as the body of one otherwise.
 *
 * @param {string} code - The source to compile.
 * @returns {vm.Script} The compiled script.
 * @throws {SyntaxError} When the code is not valid either way.
 */
function compile(code) {
  try {
    return new vm.Script(code, { filename: "<eval>" });
  } catch (error) {
    if (!(error instanceof SyntaxError) || !/await/.test(String(error.message))) {
      throw error;
    }
    try {
      return new vm.Script(`(async () => (\n${code}\n))()`, { filename: "<eval>" });
    } catch {
      return new vm.Script(`(async () => {\n${code}\n})()`, { filename: "<eval>" });
    }
  }
}

/**
 * Runs code in the shared context and waits for a promise it ends with.
 *
 * @param {string} code - The source to run.
 * @returns {Promise<{value: string | null, error: string | null}>} The inspected value of the last expression, or the error it threw.
 */
async function run(code) {
  try {
    let value = compile(code).runInContext(context);
    if (value && typeof value.then === "function") {
      value = await value;
    }
    if (value === undefined) {
      return { value: null, error: null };
    }
    context._ = value;
    return { value: util.inspect(value, { depth: 4 }), error: null };
  } catch (error) {
    const text = error instanceof Error ? error.stack || String(error) : util.inspect(error);
    return { value: null, error: text };
  }
}

/**
 * Answers requests on standard input one at a time until it closes.
 *
 * @returns {Promise<void>}
 */
async function serve() {
  const lines = readline.createInterface({ input: process.stdin, terminal: false });
  for await (const line of lines) {
    const request = JSON.parse(line);
    captured.length = 0;
    const { value, error } = await run(request.code);
    const output = captured.join("");
    process.stdout.write(`${MARKER}${JSON.stringify({ output, value, error })}\n`);
  }
}

process.on("unhandledRejection", (reason) => record("Unhandled rejection:", reason));
serve();
