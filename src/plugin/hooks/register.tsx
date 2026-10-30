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

import { onStart as identityStart, register as identity } from './identity'
import { onStart as keeperStart, register as keeper } from './keeper'
import { onTurnComplete as guideTurnComplete, register as guide } from './guide'
import { onTurnComplete as trackerTurnComplete, register as tracker } from './tracker'

export const register: Register = on => {
  // The one bare hook for each observe-only event several features need (SKEIN-1387):
  // `claude plugin validate` refuses two unmatched hooks on one event, so each fans out here,
  // in a fixed order, to what the features export, and calls `next` once, itself.
  on('session.start', async ($, e, next) => {
    await identityStart($)
    await keeperStart($)

    return next(e)
  })
  on('turn.complete', async ($, e, next) => {
    const done = await next(e)
    await guideTurnComplete($, e)
    await trackerTurnComplete($, e)

    return done
  })

  identity(on)
  keeper(on)
  guide(on)
  tracker(on)
}
