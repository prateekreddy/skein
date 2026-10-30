// The skeleton's one feature: `/skein` says which box this session is in and which version of the
// plugin it runs. Harmless, and observable from a test, from a box's pane and from `claude -p`, so
// the foundation has something to prove (SKEIN-1359).
//
// The shape every feature file follows: one exported `register(on)` that `register.tsx` calls, its
// state under its own key in `types/index.d.ts`, and its tests in `tests/<feature>.test.ts`.

import { atom, read, update } from 'claude-code'
import type { On } from 'claude-code'

import type { SkeinIdentity } from '../types'

const identity = atom({ plugin: 'skein', key: 'identity' } as const, null, {
  shape: 'identity-1',
})

export function register(on: On): void {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'skein',
      description: "Show this box's name and the skein plugin's version",
    })
    const box = (await $.env.get('SKEIN_BOX')) || null
    const version = await pluginVersion(() =>
      $.fs.read(`${$.plugin.root}/.claude-plugin/plugin.json`),
    )
    const found: SkeinIdentity = { box, version }
    await update($, identity, () => found)

    return next(e)
  })

  on('command.run', { command: 'skein' }, async $ => {
    const found = await read($, identity)

    return { text: describe(found) }
  })
}

async function pluginVersion(manifest: () => Promise<string>): Promise<string> {
  try {
    const parsed: unknown = JSON.parse(await manifest())
    if (typeof parsed === 'object' && parsed !== null && 'version' in parsed) {
      const { version } = parsed
      if (typeof version === 'string' && version !== '') return version
    }
  } catch {
    // An unreadable manifest is said as "unknown" below, not thrown: `/skein` still answers.
  }

  return 'unknown'
}

function describe(found: SkeinIdentity | null): string {
  if (found === null) return 'this session has not finished starting. Try /skein again.'
  const where =
    found.box === null ? 'not in a skein box' : `in box ${found.box}`

  // The engine heads a plugin command's output with the plugin's name, so this reads
  // `skein: version 0.1.0, in box web-main` and does not say "skein" again.
  return `version ${found.version}, ${where}`
}
