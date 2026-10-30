// `/skein`, through the whole module: `claude plugin test src/plugin` loads the plugin from its
// folder the way a session does, so these go through register.tsx, not identity.ts alone.
//
// Nothing sits beneath the plugin under test: each `on` here stands for the engine.

import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

const MANIFEST = JSON.stringify({ name: 'skein', version: '9.8.7' })

// `/skein` as the person runs it: typed at the prompt, nothing after the name.
const TYPED = {
  command: 'skein',
  args: '',
  origin: { kind: 'composer' },
  presentation: { isFullscreen: false, columns: 80 },
} as const

function engine(on: On, manifest: string | Error): void {
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('fs.read', () => {
    if (manifest instanceof Error) throw manifest

    return { value: manifest }
  })
}

test('/skein names the box and the plugin version', async ($, on) => {
  engine(on, MANIFEST)
  mock.env(on, { SKEIN_BOX: 'web-main' })

  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ran = await $.command.run(TYPED)

  expect(ran.text).toBe('version 9.8.7, in box web-main')
})

test('/skein outside a box says so rather than naming nothing', async ($, on) => {
  engine(on, MANIFEST)
  mock.env(on, {})

  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ran = await $.command.run(TYPED)

  expect(ran.text).toBe('version 9.8.7, not in a skein box')
})

test('/skein still answers when the manifest cannot be read', async ($, on) => {
  engine(on, new Error('gone'))
  mock.env(on, { SKEIN_BOX: 'web-main' })

  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ran = await $.command.run(TYPED)

  expect(ran.text).toBe('version unknown, in box web-main')
})
