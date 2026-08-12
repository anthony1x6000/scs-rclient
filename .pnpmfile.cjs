module.exports = {
  hooks: {
    readPackage(pkg) {
      if (pkg.dependencies) {
        if (pkg.dependencies.esbuild) {
          pkg.dependencies.esbuild = "^0.28.1";
        }
        if (pkg.dependencies.postcss) {
          pkg.dependencies.postcss = "^8.5.26";
        }
        if (pkg.dependencies.nanoid) {
          pkg.dependencies.nanoid = "^3.3.18";
        }
      }
      return pkg;
    },
  },
};

