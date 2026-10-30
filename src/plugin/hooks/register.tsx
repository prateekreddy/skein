// skein's hooks module: the one file `hooks/hooks.json` names under `modules`.
//
// It composes; it does nothing itself. Each feature is one file beside this one exporting
// `register(on)`, imported under the feature's name and called below in one line. A direct call
// each, not a loop over a list: `claude plugin validate` reads the module's source and refuses a
// call it cannot trace to a function the module declares or imports.
//
// The events more than one feature watches are hooked here, once, bare: a module may hook an event
// without a matcher only once (`claude plugin validate --strict` refuses a second), and one hook
// here says in what order the features hear it. Each calls `next(e)` itself, once, then hands the
// event, and what it settled to, to each feature's function in the order written. A feature whose
// part fails is passed by and the rest still hear the event. A hook that refuses or rewrites stays
// in its feature file under a matcher of its own (a tool, a command, a component), where the order
// and `next` are the feature's (docs/decisions/skein-is-a-mod.md, "The shape a feature takes").
//
// The command hooks in `hooks.json`, the monitor and the skein-mcp server load beside this module
// unchanged: a command hook is replaced only once its mod version is proven, one at a time
// (docs/decisions/skein-is-a-mod.md).

import type { Register } from 'claude-code'

import {
  register as attention,
  onEnd as attentionEnd,
  onMeasure as attentionMeasure,
  onStart as attentionStart,
  onToolCall as attentionToolCall,
  onTurnComplete as attentionTurnComplete,
  onTurnStart as attentionTurnStart,
} from './attention'
import {
  register as agentsPanel,
  onSpawn as agentsPanelSpawn,
  onStart as agentsPanelStart,
  onStep as agentsPanelStep,
  onToolCall as agentsPanelToolCall,
  onTurnComplete as agentsPanelTurnComplete,
} from './agents-panel'
import { register as identity, onStart as identityStart } from './identity'

export const register: Register = on => {
  identity(on)
  agentsPanel(on)
  attention(on)

  on('session.start', async ($, e, next) => {
    const started = await next(e)
    await quietly(identityStart($, e))
    await quietly(agentsPanelStart($, e))
    await quietly(attentionStart($, e))

    return started
  })

  on('agent.spawn', async ($, e, next) => {
    const started = await next(e)
    if (started.deny !== undefined) return started
    await quietly(agentsPanelSpawn($, e, started))

    return started
  }).catch(($, e, next) => next(e))

  on('turn.step', async function* ($, e, next) {
    const result = yield* next(e)
    await quietly(agentsPanelStep($, e, result))

    return result
  })

  on('turn.start', async ($, e, next) => {
    await quietly(attentionTurnStart($, e))

    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    await quietly(agentsPanelTurnComplete($, e))
    await quietly(attentionTurnComplete($, e))

    return next(e)
  })

  on('tool.call', async ($, e, next) => {
    const answered = await next(e)
    await quietly(agentsPanelToolCall($, e, answered))
    await quietly(attentionToolCall($, e))

    return answered
  }).catch(($, e, next) => next(e))

  on('session.measure', async ($, e, next) => {
    await quietly(attentionMeasure($, e))

    return next(e)
  })

  on('session.end', async ($, e, next) => {
    await quietly(attentionEnd($, e))

    return next(e)
  })
}

// One feature's part of an event: a failure stays the feature's, and the next part still runs.
async function quietly(part: Promise<void>): Promise<void> {
  try {
    await part
  } catch {
    // Passed by: the event is not the feature's to fail.
  }
}
