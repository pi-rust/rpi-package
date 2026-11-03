(function(start, log, emit, select, catalogJSON) {
  "use strict";
  // Capture bridge dependencies before guest code can replace their globals or
  // prototypes. Native closures and request tokens stay inside this lexical scope.
  const parse = JSON.parse, stringify = JSON.stringify, String_ = String;
  const Error_ = Error, Promise_ = Promise, isArray = Array.isArray;
  const freeze = Object.freeze, define = Object.defineProperty;
  const pending = new Map();
  const get = pending.get.bind(pending), set = pending.set.bind(pending);
  const remove = pending.delete.bind(pending);
  const finalized = new WeakMap();
  const mark = finalized.set.bind(finalized), nativeError = finalized.get.bind(finalized);
  const catalog = parse(catalogJSON);
  const tools = Object.create(null);
  const call = freeze((name, args) => new Promise_((resolve, reject) => {
    if (typeof name !== "string") throw new Error_("tool name must be a string");
    if (args === null || typeof args !== "object" || isArray(args))
      throw new Error_("tool arguments must be an object");
    const token = start(name, stringify(args));
    set(token, {resolve, reject});
  }));
  tools.call = call;
  tools.emit = freeze(value => {
    let encoded;
    try { encoded = stringify(value); }
    catch (error) {
      // Latch the native fatal path using only a fixed string, never the value
      // that failed serialization. Catching this exception cannot restore success.
      emit("invalid emitted JSON");
      throw error;
    }
    // JSON.stringify(undefined/functions/symbols) returns undefined without
    // throwing; still enter native validation rather than failing conversion
    // before the sticky bridge latch.
    return emit(typeof encoded === "string" ? encoded : "invalid emitted JSON");
  });
  tools.result = freeze(value => {
    const isError = nativeError(value);
    if (typeof isError !== "boolean")
      return select("invalid native result selection", false);
    let encoded;
    try { encoded = stringify({content:value.content, details:value.details}); }
    catch (error) {
      select("invalid native result selection", false);
      throw error;
    }
    return select(typeof encoded === "string" ? encoded : "invalid native result selection", isError);
  });
  tools.list = freeze(() => parse(catalogJSON));
  for (const tool of catalog) {
    if (tool.name !== "call" && tool.name !== "list" && tool.name !== "emit" && tool.name !== "result")
      tools[tool.name] = freeze(args => call(tool.name, args));
  }
  const expose = (name, value) => define(globalThis, name,
    {value: freeze(value), writable: false, configurable: false});
  expose("tools", tools);
  const output = level => freeze((...args) => {
    let text = "";
    for (let i = 0; i < args.length; i++) {
      if (i) text += " ";
      const x = args[i];
      text += typeof x === "string" ? x : stringify(x) ?? String_(x);
    }
    log(level, text);
  });
  expose("print", output("print"));
  expose("console", {log:output("log"), info:output("info"),
    warn:output("warn"), error:output("error")});
  return (token, ok, payload) => {
    const entry = get(token);
    remove(token);
    if (ok) {
      const result = parse(payload);
      mark(result, false);
      entry.resolve(result);
    }
    else {
      const failure = parse(payload);
      const error = new Error_("tool returned isError: " + failure.message);
      error.result = failure.result;
      mark(error.result, true);
      entry.reject(error);
    }
  };
})
