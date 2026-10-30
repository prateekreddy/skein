// skein's hooks module: the one file `hooks/hooks.json` names under `modules`.
//
// It composes; it does nothing itself. Each feature is one file beside this one exporting
// `register(on)`, imported under the feature's name and called below in one line. A direct call
// each, not a loop over a list: `claude plugin validate` reads the module's source and refuses a
// call it cannot trace to a function the module declares or imports.
//
// The command hooks in `hooks.json`, the monitor and the skein-mcp server load beside this module
// unchanged: a command hook is replaced only once its mod version is proven, one at a time
// (docs/decisions/skein-is-a-mod.md).

import type { Register } from 'claude-code'

import { onStart as bandStart, register as band } from './band'
import { onStart as identityStart, register as identity } from './identity'

export const register: Register = on => {
  // The one bare hook of each observe-only event more than one feature needs, fanning out in a
  // fixed order and calling `next(e)` once (SKEIN-1387): `claude plugin validate` refuses a second
  // bare hook of one event in a module.
  on('session.start', async ($, e, next) => {
    await identityStart($)
    await bandStart($)

    return next(e)
  })

  identity(on)
  band(on)
}
