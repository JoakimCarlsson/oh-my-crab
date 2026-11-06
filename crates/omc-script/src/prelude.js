/**
 * Sets up everything a script can reach, the tools, printing and the error type of failed calls, and returns the functions the host drives it with.
 * @param {object} host The host's functions: `call(name, json)` starts a tool call and returns its number, `print(text)` adds a line to the output.
 * @param {string[]} names The tools the script may call.
 * @returns {object} `settle`, `render` and `failure`, for the host.
 */
(function prelude(host, names) {
  "use strict";
  const pending = new Map();

  /** A tool call that did not succeed, with `tool` naming the tool. */
  class ToolError extends Error {
    /**
     * Makes the error of a failed call.
     * @param {string} tool The tool that was called.
     * @param {string} message What went wrong, as the tool or the harness said it.
     */
    constructor(tool, message) {
      super(message);
      this.name = "ToolError";
      this.tool = tool;
    }
  }

  /**
   * Turns maps into plain objects and sets into arrays while a value is written as JSON, which would otherwise show them empty.
   * @param {string} key The key of the value being written.
   * @param {*} value The value being written.
   * @returns {*} The value to write instead.
   */
  function plain(key, value) {
    if (value instanceof Map) {
      return Object.fromEntries(value);
    }
    if (value instanceof Set) {
      return [...value];
    }
    return value;
  }

  /**
   * Shows a value as text: strings as they are, errors with their name and message, the rest as indented JSON when they have a JSON form.
   * @param {*} value What to show.
   * @returns {string} The text.
   */
  function show(value) {
    if (typeof value === "string") {
      return value;
    }
    if (value instanceof Error) {
      return `${value.name}: ${value.message}`;
    }
    try {
      const json = JSON.stringify(value, plain, 2);
      return json === undefined ? String(value) : json;
    } catch {
      return String(value);
    }
  }

  /**
   * Makes the function that calls the tool `name`.
   * @param {string} name The tool.
   * @returns {function(object=): Promise<*>} Calls the tool with one object of arguments and resolves to its result.
   */
  function caller(name) {
    return (args = {}) =>
      new Promise((resolve, reject) => {
        if (args === null || typeof args !== "object" || Array.isArray(args)) {
          throw new TypeError(`tools.${name} takes one object of arguments`);
        }
        const id = host.call(name, JSON.stringify(args));
        pending.set(id, { name, resolve, reject });
      });
  }

  /**
   * Makes the function standing in for a tool scripts do not have, whose calls fail naming the tools they do have.
   * @param {string} name The name the script used.
   * @returns {function(): Promise<never>} A function whose calls reject.
   */
  function missing(name) {
    const known = names.length ? names.join(", ") : "none";
    return () =>
      Promise.reject(new ToolError(name, `there is no tool called ${name} in scripts; the tools are: ${known}`));
  }

  /**
   * Adds the values, shown as text and joined by spaces, as one line of the output.
   * @param {...*} values What to print.
   */
  function print(...values) {
    host.print(values.map(show).join(" "));
  }

  const table = Object.create(null);
  for (const name of names) {
    table[name] = caller(name);
  }
  Object.freeze(table);
  const tools = new Proxy(table, {
    get: (target, key) =>
      typeof key !== "string" || key in target || key === "then" ? target[key] : missing(key),
  });
  const console = Object.freeze({ log: print, info: print, warn: print, error: print, debug: print });
  for (const [key, value] of Object.entries({ tools, print, console, ToolError })) {
    Object.defineProperty(globalThis, key, { value, writable: false, configurable: false, enumerable: false });
  }

  return {
    /**
     * Ends the tool call numbered `id`: resolves it with the result, or rejects it with a `ToolError`.
     * @param {number} id The number `host.call` gave the call.
     * @param {boolean} ok Whether the call succeeded.
     * @param {string} json The result as JSON when it succeeded, or the error message as a JSON string.
     */
    settle(id, ok, json) {
      const waiting = pending.get(id);
      if (!waiting) {
        return;
      }
      pending.delete(id);
      const payload = JSON.parse(json);
      if (ok) {
        waiting.resolve(payload);
      } else {
        waiting.reject(new ToolError(waiting.name, payload));
      }
    },

    /**
     * Shows what the script returned, or nothing when it returned nothing.
     * @param {*} value The value.
     * @returns {string|undefined} The text, or `undefined`.
     */
    render(value) {
      return value === undefined ? undefined : show(value);
    },

    /**
     * Describes what the script threw: an error with its name, message and stack, anything else as text.
     * @param {*} error What was thrown.
     * @returns {string} The description.
     */
    failure(error) {
      if (error instanceof Error) {
        const stack = typeof error.stack === "string" ? error.stack.trimEnd() : "";
        return stack ? `${error.name}: ${error.message}\n${stack}` : `${error.name}: ${error.message}`;
      }
      return `uncaught ${show(error)}`;
    },
  };
})
