// nativescript.hmr / nativescript.livesync: module cache control for tooling.
(function (natives) {
  'use strict';
  const ns = globalThis.nativescript;
  const hmr = {
    resolvePath: (specifier, parentPath) => natives.hmrResolve(String(specifier), parentPath),
    invalidate: (path) => natives.hmrInvalidate(hmr.resolvePath(path)),
    clear: () => natives.hmrClear(),
    /** Reloads a module from disk and returns its exports. */
    applyFromFile(path) {
      const resolved = hmr.resolvePath(path);
      natives.hmrInvalidate(resolved);
      return natives.hmrLoad(resolved);
    },
  };

  const livesync = {
    /** Copies a built file into place, then reloads it. */
    sync(sourcePath, destPath) {
      if (!sourcePath || !destPath) throw new Error('livesync.sync(sourcePath, destPath) needs two paths');
      natives.copyFile(String(sourcePath), String(destPath));
      return hmr.applyFromFile(destPath);
    },
    reload: (path) => hmr.applyFromFile(path),
    reset: () => hmr.clear(),
  };

  Object.defineProperty(ns, 'hmr', { value: hmr, enumerable: true });
  Object.defineProperty(ns, 'livesync', { value: livesync, enumerable: true });

  // `ns:module`: dev host primitives for @nativescript/vite (policy stays in JS there)
  if (natives.devSetBuiltin) {
    natives.devSetBuiltin(
      Object.freeze({
        configureLoader: (config) => natives.devConfigure(JSON.stringify(config ?? {})),
        invalidateModules: (urls) => natives.devInvalidate(Array.from(urls ?? [], String)),
        getLoadedModuleUrls: () => natives.devLoaded(),
        createRequire: (file) => natives.devCreateRequire(String(file?.href ?? file)),
      }),
    );
  }
});
