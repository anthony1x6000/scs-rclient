module.exports = {
  hooks: {
    readPackage(pkg) {
      for (const kind of ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"]) {
        const deps = pkg[kind];
        if (!deps) continue;
        if (deps.esbuild) {
          deps.esbuild = "^0.28.1";
        }
        if (deps.postcss) {
          deps.postcss = "^8.5.26";
        }
        if (deps.nanoid) {
          deps.nanoid = "^3.3.18";
        }
        if (deps.browserslist) {
          deps.browserslist = "^4.28.7";
        }
      }
      return pkg;
    },
  },
};
