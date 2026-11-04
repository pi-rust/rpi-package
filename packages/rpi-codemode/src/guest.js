(function(start, log, emit, select, referenceOps, catalogJSON) {
  "use strict";
  // Capture bridge dependencies before guest code can replace their globals or
  // prototypes. Native closures and request tokens stay inside this lexical scope.
  const read = referenceOps.read, release = referenceOps.release, selectRef = referenceOps.select;
  const parse = JSON.parse, stringify = JSON.stringify, String_ = String;
  const Error_ = Error, Promise_ = Promise, isArray = Array.isArray;
  const freeze = Object.freeze, define = Object.defineProperty;
  const keys = Object.keys, create = Object.create;
  const pending = new Map();
  const get = pending.get.bind(pending), set = pending.set.bind(pending);
  const remove = pending.delete.bind(pending);
  const finalized = new WeakMap();
  const mark = finalized.set.bind(finalized), nativeError = finalized.get.bind(finalized);
  const catalog = parse(catalogJSON);
  const tools = create(null);
  // Only plain parsed JSON is traversed, using captured intrinsics and indexed
  // work lists: guest-modified prototypes/iterators cannot intercept freezing.
  const deepFreeze = root => {
    const work = [root];
    for (let i = 0; i < work.length; i++) {
      const node = work[i];
      if (node === null || typeof node !== "object") continue;
      const names = keys(node);
      for (let j = 0; j < names.length; j++) {
        const child = node[names[j]];
        if (child !== null && typeof child === "object") {
          const descriptor = create(null);
          descriptor.value = child;
          descriptor.writable = descriptor.enumerable = descriptor.configurable = true;
          define(work, work.length, descriptor);
        }
      }
      freeze(node);
    }
    return root;
  };
  const receipt = token => {
    const value = create(null);
    let loaded;
    const fields = ["content", "details", "usage", "addedToolNames", "terminate"];
    for (let i = 0; i < fields.length; i++) {
      const field = fields[i];
      const descriptor = create(null);
      descriptor.enumerable = true;
      descriptor.configurable = false;
      descriptor.get = freeze(() => {
        // Even cached reads must validate lifetime after explicit release.
        const text = read(token, loaded === undefined);
        if (loaded === undefined) loaded = deepFreeze(parse(text));
        return loaded[field];
      });
      define(value, field, descriptor);
    }
    return freeze(value);
  };
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
    const provenance = nativeError(value);
    if (provenance === undefined)
      return select("invalid native result selection", false);
    if (provenance.token) return selectRef(provenance.token);
    let encoded;
    try { encoded = stringify({content:value.content, details:value.details}); }
    catch (error) {
      select("invalid native result selection", false);
      throw error;
    }
    return select(typeof encoded === "string" ? encoded : "invalid native result selection", provenance.isError);
  });
  tools.release = freeze(value => {
    const provenance = nativeError(value);
    if (provenance === undefined) throw new Error_("tools.release requires a native result");
    return release(provenance.token);
  });
  tools.list = freeze(() => parse(catalogJSON));
  for (const tool of catalog) {
    if (tool.name !== "call" && tool.name !== "list" && tool.name !== "emit" && tool.name !== "result" && tool.name !== "release")
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
  return (token, ok, payload, large) => {
    const entry = get(token);
    remove(token);
    const decoded = large ? undefined : parse(payload);
    const result = large ? receipt(token) : (ok ? decoded : decoded.result);
    mark(result, {isError:!ok, token:large ? token : 0});
    if (ok) entry.resolve(result);
    else {
      let message = "finalized tool error; inspect error.result";
      if (!large) message = decoded.message;
      const error = new Error_("tool returned isError: " + message);
      error.result = result;
      entry.reject(error);
    }
  };
})
