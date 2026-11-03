(function(start, log, catalogJSON) {
  "use strict";
  // Capture bridge dependencies before guest code can replace their globals or
  // prototypes. Native closures and request tokens stay inside this lexical scope.
  const parse = JSON.parse, stringify = JSON.stringify, String_ = String;
  const Error_ = Error, Promise_ = Promise, isArray = Array.isArray;
  const freeze = Object.freeze, define = Object.defineProperty;
  const pending = new Map();
  const get = pending.get.bind(pending), set = pending.set.bind(pending);
  const remove = pending.delete.bind(pending);
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
  tools.list = freeze(() => parse(catalogJSON));
  for (const tool of catalog) {
    if (tool.name !== "call" && tool.name !== "list")
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
    if (ok) entry.resolve(parse(payload));
    else entry.reject(new Error_(payload));
  };
})
