# cockpit

The cockpit's pure functions, as modules that can be imported — and a build that turns them into the
one bundle the page loads.

**Why this exists.** §11.7: "The current single 6516-line `index.html` cannot support both the
pure-function testing law (§13) and a reviewed component library." Functions in that file are
genuinely pure and genuinely worth testing, and none of them could be imported — so the only way to
test one was to reimplement it in Rust and assert the page contained a string. A re-implementation
agrees with the code right up until one of them changes.

**The rule these modules keep: no imports.** `build.mjs` refuses a module containing an `import`
statement, and that is a design constraint rather than a limitation of the build. What belongs here
is leaf functions — all inputs as arguments, no module state, no DOM — which is exactly the set §13's
law is about. A function that needs another module is a function that has grown a dependency, and it
should either take it as an argument or stay in the page.

**One bundle, self-contained.** The cockpit is served from a binary to a browser on the same machine
and has no business fetching from a CDN — the same reason `xterm` and `marked` are vendored.

```
node cockpit/build.mjs      # rewrite src/web/vendor/cockpit.js from cockpit/src
node --test cockpit/test    # the pure functions, in node
```

The built bundle is committed, because the binary embeds it and `cargo build` does not run node. A
Rust test rebuilds it in memory and compares, so a stale bundle is a failing test rather than a
cockpit that quietly runs last week's code.
