// Preloaded into lhci (`node --require`): chrome-launcher's WSL branch gives this Linux Chrome a
// Windows temp dir (`undefined:/Users/undefined/AppData/...`). Chrome here is Linux: is-wsl -> false.
// lighthouse's ESM chrome-launcher loads it by absolute path, lhci's CJS one by name: match both.
const Module = require('node:module');
const load = Module._load;
Module._load = function (request, ...rest) {
  return /^is-wsl$|[\\/]is-wsl[\\/]index\.js$/.test(request) ? false : load.call(this, request, ...rest);
};
